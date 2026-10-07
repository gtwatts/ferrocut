# ferrocut-html-host: CEF (BSD-3) windowless renderer process.
# Configure with -DCEF_ROOT=<cef_binary dir>; see ../scripts/fetch-cef.sh.
cmake_minimum_required(VERSION 3.21)
project(ferrocut_html_host CXX C)

if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()
set(CEF_ROOT "" CACHE PATH "CEF binary distribution")
list(APPEND CMAKE_MODULE_PATH "${CEF_ROOT}/cmake")
find_package(CEF REQUIRED)
# CEF's default "-Wl,-rpath,." would let the loader pick libraries up from the
# current working directory. Use exactly $ORIGIN (set on the target below).
list(REMOVE_ITEM CEF_LINKER_FLAGS "-Wl,-rpath,.")

add_subdirectory(${CEF_LIBCEF_DLL_WRAPPER_PATH} libcef_dll_wrapper)

add_executable(ferrocut-html-host src/main.cpp src/shim.h)
SET_EXECUTABLE_TARGET_PROPERTIES(ferrocut-html-host)
add_dependencies(ferrocut-html-host libcef_dll_wrapper)
ADD_LOGICAL_TARGET("libcef_lib" "${CEF_LIB_DEBUG}" "${CEF_LIB_RELEASE}")
target_link_libraries(ferrocut-html-host libcef_lib libcef_dll_wrapper ${CEF_STANDARD_LIBS})
set_target_properties(ferrocut-html-host PROPERTIES
  BUILD_WITH_INSTALL_RPATH TRUE
  INSTALL_RPATH "\$ORIGIN"
  RUNTIME_OUTPUT_DIRECTORY "${CMAKE_CURRENT_BINARY_DIR}")

# CEF wants libcef.so, its snapshot/ICU data and resources next to the
# executable. Symlink rather than copy (libcef.so is ~1.4 GB).
set(_out "${CMAKE_CURRENT_BINARY_DIR}")
# Relative link targets, so moving or renaming the checkout keeps them valid.
foreach(f ${CEF_BINARY_FILES})
  file(RELATIVE_PATH _rel "${_out}" "${CEF_BINARY_DIR_RELEASE}/${f}")
  add_custom_command(TARGET ferrocut-html-host POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E rm -f "${_out}/${f}"
    COMMAND ${CMAKE_COMMAND} -E create_symlink "${_rel}" "${_out}/${f}")
endforeach()
foreach(f ${CEF_RESOURCE_FILES})
  file(RELATIVE_PATH _rel "${_out}" "${CEF_RESOURCE_DIR}/${f}")
  add_custom_command(TARGET ferrocut-html-host POST_BUILD
    COMMAND ${CMAKE_COMMAND} -E rm -f "${_out}/${f}"
    COMMAND ${CMAKE_COMMAND} -E create_symlink "${_rel}" "${_out}/${f}")
endforeach()
