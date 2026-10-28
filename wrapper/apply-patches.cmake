# Applies wrapper/patches/*.patch, in order, to the clap-wrapper checkout (run by
# FetchContent in that checkout, with -DPATCH_DIR=...). It runs again whenever a patch is
# added or changed, so it first puts the tracked files back to the pinned commit: a later
# patch may change lines an earlier one added, and a changed patch must not meet an older
# version of itself. Each patch is a fix meant to go upstream; drop it once the pinned
# clap-wrapper contains it.
execute_process(COMMAND git checkout -- . RESULT_VARIABLE failed)
if (failed)
    message(FATAL_ERROR "apf: cannot restore the clap-wrapper checkout before patching")
endif()
file(GLOB patches "${PATCH_DIR}/*.patch")
list(SORT patches)
foreach (patch IN LISTS patches)
    execute_process(COMMAND git apply "${patch}" RESULT_VARIABLE failed)
    if (failed)
        message(FATAL_ERROR "apf: cannot apply ${patch} to clap-wrapper")
    endif()
    message(STATUS "apf: applied ${patch}")
endforeach()
