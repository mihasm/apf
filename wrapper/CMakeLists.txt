# Builds every plugin format from one Rust static library with free-audio/clap-wrapper
# ("clap first"): the CLAP itself, and VST3, AUv2, AUv3 and a standalone app that wrap it.
# Each format links the same static library, so every bundle is self-contained.
#
# Driven by `cargo xtask bundle`, which builds the Rust library and passes:
#   APF_STATICLIB       path of lib<plugin>.a
#   APF_NATIVE_LIBS     what the Rust library needs at link time (rustc --print native-static-libs)
#   APF_NAME            display / bundle name ("APF Demo Synth")
#   APF_TARGET          CMake-safe target name
#   APF_BUNDLE_ID       reverse-DNS bundle id
#   APF_CLAP_ID         the plugin's CLAP id (for the standalone)
#   APF_VERSION         x.y.z
#   APF_FORMATS         list of CLAP VST3 AUV2 AUV3
#   APF_STANDALONE      ON/OFF
#   APF_AU_TYPE, APF_AU_SUBTYPE, APF_AU_MANUFACTURER, APF_AU_MANUFACTURER_NAME
#   APF_ASSET_DIR       where the finished bundles go
cmake_minimum_required(VERSION 3.24)

if (NOT DEFINED CMAKE_OSX_DEPLOYMENT_TARGET)
    # The Rust aarch64-apple-darwin target's own minimum.
    set(CMAKE_OSX_DEPLOYMENT_TARGET 11.0 CACHE STRING "")
endif()

project(apf-wrapper VERSION ${APF_VERSION} LANGUAGES C CXX)
if (APPLE)
    # The wrappers' Cocoa parts are Objective-C++; the languages must be enabled up here.
    enable_language(OBJC)
    enable_language(OBJCXX)
endif()
set(CMAKE_CXX_STANDARD 17)
if (MSVC)
    # The static C runtime in every part, Debug builds too: the Rust library is built with
    # +crt-static (Rust never links the debug runtime), and a plugin then needs no Visual C++
    # redistributable on the user's machine. clap-wrapper keeps a value set here.
    set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded")
endif()

include(FetchContent)
# A hash of the patches is part of the patch command, so adding or changing a patch re-runs
# the patch step on an existing checkout (FetchContent only re-runs a step whose command
# changed); apply-patches.cmake starts again from the pinned source each time.
file(GLOB apf_patches "${CMAKE_CURRENT_SOURCE_DIR}/patches/*.patch")
list(SORT apf_patches)
set(apf_patch_hashes "")
foreach (patch IN LISTS apf_patches)
    file(SHA256 "${patch}" hash)
    string(APPEND apf_patch_hashes "${hash}")
endforeach()
string(SHA256 apf_patches_hash "${apf_patch_hashes}")
# Pinned: clap-wrapper v0.17.0 (MIT). It downloads, also pinned, the CLAP SDK (MIT),
# the VST3 SDK (MIT since 3.8) and Apple's AudioUnitSDK (Apache-2.0).
FetchContent_Declare(clap-wrapper
        GIT_REPOSITORY https://github.com/free-audio/clap-wrapper.git
        GIT_TAG 806bba0a5f792e3966b99c1561ab9952d3316d7c
        GIT_SHALLOW FALSE
        # Our fixes to it, until they are upstream (see patches/).
        PATCH_COMMAND ${CMAKE_COMMAND} -DPATCH_DIR=${CMAKE_CURRENT_SOURCE_DIR}/patches
                -DPATCHES_HASH=${apf_patches_hash} -P ${CMAKE_CURRENT_SOURCE_DIR}/apply-patches.cmake
        UPDATE_DISCONNECTED TRUE)
set(CLAP_WRAPPER_DOWNLOAD_DEPENDENCIES TRUE CACHE BOOL "" FORCE)
# clap-wrapper keeps its own location in the cache (CLAP_WRAPPER_CMAKE_CURRENT_SOURCE_DIR)
# and compiles every format from the sources there; a cache entry is never overwritten. A
# build folder first configured when the checkout lived elsewhere (all builds once shared
# one) kept the old path and went on compiling that checkout — its older patches, not the
# ones applied above. Dropped here, clap-wrapper records where it is now.
unset(CLAP_WRAPPER_CMAKE_CURRENT_SOURCE_DIR CACHE)
FetchContent_MakeAvailable(clap-wrapper)
file(REAL_PATH "${CLAP_WRAPPER_CMAKE_CURRENT_SOURCE_DIR}" apf_wrapper_compiled)
file(REAL_PATH "${clap-wrapper_SOURCE_DIR}" apf_wrapper_patched)
if (NOT apf_wrapper_compiled STREQUAL apf_wrapper_patched)
    message(FATAL_ERROR "apf: clap-wrapper would compile ${apf_wrapper_compiled}, not the patched checkout ${apf_wrapper_patched}")
endif()

# The Rust side: an imported static library plus the system libraries it needs.
add_library(apf-rust STATIC IMPORTED)
set_target_properties(apf-rust PROPERTIES IMPORTED_LOCATION "${APF_STATICLIB}")
set(native_libs "")
set(take_framework FALSE)
foreach (item IN LISTS APF_NATIVE_LIBS)
    if (take_framework)
        list(APPEND native_libs "-framework ${item}")
        set(take_framework FALSE)
    elseif (item STREQUAL "-framework")
        set(take_framework TRUE)
    else()
        list(APPEND native_libs "${item}")
    endif()
endforeach()

add_library(${APF_TARGET}-impl INTERFACE)
target_link_libraries(${APF_TARGET}-impl INTERFACE apf-rust ${native_libs} clap)
if (APPLE)
    # Drop the parts of the Rust library a format does not use.
    target_link_options(${APF_TARGET}-impl INTERFACE -Wl,-dead_strip)
endif()

set(standalone "")
if (APF_STANDALONE)
    set(standalone STANDALONE_CONFIGURATIONS standalone "${APF_NAME}" "${APF_CLAP_ID}")
endif()

make_clapfirst_plugins(
        TARGET_NAME ${APF_TARGET}
        IMPL_TARGET ${APF_TARGET}-impl
        OUTPUT_NAME "${APF_NAME}"
        ENTRY_SOURCE "${CMAKE_CURRENT_SOURCE_DIR}/entry.cpp"
        BUNDLE_IDENTIFIER "${APF_BUNDLE_ID}"
        BUNDLE_VERSION "${APF_VERSION}"
        COPY_AFTER_BUILD FALSE
        # Windows: the VST3 as a bundle folder (Contents/x86_64-win/), as the VST3 SDK
        # recommends, not the legacy single file.
        WINDOWS_FOLDER_VST3 TRUE
        PLUGIN_FORMATS ${APF_FORMATS}
        ASSET_OUTPUT_DIRECTORY "${APF_ASSET_DIR}"
        AUV2_MANUFACTURER_NAME "${APF_AU_MANUFACTURER_NAME}"
        AUV2_MANUFACTURER_CODE "${APF_AU_MANUFACTURER}"
        AUV2_SUBTYPE_CODE "${APF_AU_SUBTYPE}"
        AUV2_INSTRUMENT_TYPE "${APF_AU_TYPE}"
        ${standalone}
)

if (TARGET ${APF_TARGET}_auv3 AND NOT CMAKE_SYSTEM_NAME STREQUAL "iOS")
    # make_clapfirst_plugins has no way to pass the AUv3's entitlements; the web editor needs
    # more than clap-wrapper's default (see auv3.entitlements). Re-sign after its own signing
    # step (post-build commands run in the order they were added), before the host app
    # embeds the extension.
    set(apf_entitlements "${CMAKE_CURRENT_SOURCE_DIR}/auv3.entitlements")
    set_target_properties(${APF_TARGET}_auv3 PROPERTIES
            XCODE_ATTRIBUTE_CODE_SIGN_ENTITLEMENTS "${apf_entitlements}")
    add_custom_command(TARGET ${APF_TARGET}_auv3 POST_BUILD
            COMMAND codesign -s - -f --entitlements "${apf_entitlements}" "$<TARGET_BUNDLE_DIR:${APF_TARGET}_auv3>"
            COMMENT "Signing the AUv3 appex with the web editor's entitlements")
endif()
