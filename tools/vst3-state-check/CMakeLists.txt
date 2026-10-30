# vst3-state-check — a headless VST3 host that checks that a restored session sounds as saved.
#
#   cmake -S tools/vst3-state-check -B <build> -DVST3_SDK_ROOT=<vst3sdk> -DCMAKE_BUILD_TYPE=Release
#   cmake --build <build> --config Release
#
# VST3_SDK_ROOT is the VST3 SDK (3.8 or later, MIT) the wrapper build downloads into CPM's
# source cache: ~/.cargo-target/apf-wrapper/cpm-cache/vst3sdk/<hash>. `cargo xtask validate`
# builds this tool from there by itself.
cmake_minimum_required(VERSION 3.25)

if(NOT DEFINED ENV{MACOSX_DEPLOYMENT_TARGET})
    set(CMAKE_OSX_DEPLOYMENT_TARGET "11.0" CACHE STRING "macOS deployment target")
endif()

project(vst3-state-check LANGUAGES C CXX)
if(APPLE)
    enable_language(OBJCXX)
endif()

set(VST3_SDK_ROOT "" CACHE PATH "The VST3 SDK (3.8 or later)")
if(NOT EXISTS "${VST3_SDK_ROOT}/public.sdk/source/vst/hosting/module.h")
    message(FATAL_ERROR "VST3_SDK_ROOT must name the VST3 SDK (got '${VST3_SDK_ROOT}')")
endif()

# Only the SDK's libraries: no examples, no VSTGUI, no wrappers.
set(SMTG_ENABLE_VST3_PLUGIN_EXAMPLES OFF CACHE BOOL "" FORCE)
set(SMTG_ENABLE_VST3_HOSTING_EXAMPLES OFF CACHE BOOL "" FORCE)
set(SMTG_ENABLE_VSTGUI_SUPPORT OFF CACHE BOOL "" FORCE)
set(SMTG_ENABLE_AUV2_BUILDS OFF CACHE BOOL "" FORCE)
set(SMTG_BUILD_UNIVERSAL_BINARY OFF CACHE BOOL "" FORCE)
set(SMTG_CREATE_PLUGIN_LINK OFF CACHE BOOL "" FORCE)
set(SMTG_RUN_VST_VALIDATOR OFF CACHE BOOL "" FORCE)
add_subdirectory("${VST3_SDK_ROOT}" vst3sdk EXCLUDE_FROM_ALL)

set(sources
    source/main.cpp
    source/platform.h
    "${VST3_SDK_ROOT}/public.sdk/source/common/memorystream.cpp"
    "${VST3_SDK_ROOT}/public.sdk/source/vst/hosting/plugprovider.cpp"
)
if(APPLE)
    list(APPEND sources
        source/platform_mac.mm
        "${VST3_SDK_ROOT}/public.sdk/source/vst/hosting/module_mac.mm")
    set_source_files_properties(source/platform_mac.mm
        "${VST3_SDK_ROOT}/public.sdk/source/vst/hosting/module_mac.mm"
        PROPERTIES COMPILE_FLAGS "-fobjc-arc")
elseif(WIN32)
    list(APPEND sources
        source/platform_win.cpp
        "${VST3_SDK_ROOT}/public.sdk/source/vst/hosting/module_win32.cpp")
else()
    list(APPEND sources
        source/platform_linux.cpp
        "${VST3_SDK_ROOT}/public.sdk/source/vst/hosting/module_linux.cpp")
endif()

add_executable(vst3-state-check ${sources})
target_compile_features(vst3-state-check PRIVATE cxx_std_17)
target_link_libraries(vst3-state-check PRIVATE sdk_hosting)
if(APPLE)
    target_link_libraries(vst3-state-check PRIVATE "-framework Cocoa")
elseif(WIN32)
    target_link_libraries(vst3-state-check PRIVATE user32)
    if(MSVC)
        target_compile_options(vst3-state-check PRIVATE /utf-8)
    endif()
else()
    target_link_libraries(vst3-state-check PRIVATE dl pthread)
endif()
# The executable lands in the build folder itself, also with multi-config generators.
set_target_properties(vst3-state-check PROPERTIES RUNTIME_OUTPUT_DIRECTORY "$<1:${CMAKE_BINARY_DIR}>")
