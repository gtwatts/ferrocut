# ferrocut-ofx-host + test plugin bundles.
# Driven by crates/ferrocut-ofx/build.rs through the `cmake` crate; can also be
# built by hand:
#   cmake -S host -B build -DOPENFX_DIR=third_party/src-openfx -DEXPAT_DIR=third_party/src-expat
cmake_minimum_required(VERSION 3.16)
project(ferrocut_ofx_host C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

set(OPENFX_DIR "" CACHE PATH "OpenFX source tree (pinned tag, see scripts/fetch-deps.sh)")
set(EXPAT_DIR "" CACHE PATH "libexpat source tree (pinned tag, see scripts/fetch-deps.sh)")
if(NOT EXISTS "${OPENFX_DIR}/HostSupport/src/ofxhHost.cpp")
  message(FATAL_ERROR "OPENFX_DIR='${OPENFX_DIR}' is not an OpenFX checkout")
endif()
if(NOT EXISTS "${EXPAT_DIR}/expat/CMakeLists.txt")
  message(FATAL_ERROR "EXPAT_DIR='${EXPAT_DIR}' is not a libexpat checkout")
endif()

# --- libexpat (static; HostSupport parses its plugin cache XML with it)
set(EXPAT_BUILD_TOOLS OFF CACHE BOOL "" FORCE)
set(EXPAT_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)
set(EXPAT_BUILD_TESTS OFF CACHE BOOL "" FORCE)
set(EXPAT_BUILD_DOCS OFF CACHE BOOL "" FORCE)
set(EXPAT_BUILD_FUZZERS OFF CACHE BOOL "" FORCE)
set(EXPAT_BUILD_PKGCONFIG OFF CACHE BOOL "" FORCE)
set(EXPAT_SHARED_LIBS OFF CACHE BOOL "" FORCE)
set(EXPAT_ENABLE_INSTALL OFF CACHE BOOL "" FORCE)
add_subdirectory("${EXPAT_DIR}/expat" expat EXCLUDE_FROM_ALL)

# --- OpenFX HostSupport (BSD-3-Clause)
file(GLOB OFX_HOSTSUPPORT_SRC "${OPENFX_DIR}/HostSupport/src/*.cpp")
add_library(OfxHostSupport STATIC ${OFX_HOSTSUPPORT_SRC})
target_include_directories(OfxHostSupport PUBLIC "${OPENFX_DIR}/include" "${OPENFX_DIR}/HostSupport/include")
target_link_libraries(OfxHostSupport PUBLIC expat::expat ${CMAKE_DL_LIBS})
target_compile_options(OfxHostSupport PRIVATE -w)  # upstream code, not ours to lint

# --- the host process
add_executable(ferrocut-ofx-host src/main.cpp src/host.cpp)
target_link_libraries(ferrocut-ofx-host PRIVATE OfxHostSupport)
target_compile_options(ferrocut-ofx-host PRIVATE -Wall -Wextra -Wno-unused-parameter)
install(TARGETS ferrocut-ofx-host RUNTIME DESTINATION bin)

# --- plugin bundles: <Name>.ofx.bundle/Contents/Linux-x86-64/<Name>.ofx
function(ferrocut_ofx_bundle target name)
  set_target_properties(${target} PROPERTIES PREFIX "" SUFFIX ".ofx" OUTPUT_NAME "${name}"
    CXX_VISIBILITY_PRESET hidden)
  install(TARGETS ${target} LIBRARY DESTINATION "plugins/${name}.ofx.bundle/Contents/Linux-x86-64")
endfunction()

# OpenFX C++ Support library + the SDK's float-capable Invert example.
file(GLOB OFX_SUPPORT_SRC "${OPENFX_DIR}/Support/Library/*.cpp")
add_library(OfxSupport STATIC ${OFX_SUPPORT_SRC})
target_include_directories(OfxSupport PUBLIC "${OPENFX_DIR}/include" "${OPENFX_DIR}/Support/include")
target_compile_options(OfxSupport PRIVATE -w)

add_library(ofx_invert MODULE "${OPENFX_DIR}/Support/Plugins/Invert/invert.cpp")
target_include_directories(ofx_invert PRIVATE "${OPENFX_DIR}/Support/Plugins/include")
target_link_libraries(ofx_invert PRIVATE OfxSupport)
target_compile_options(ofx_invert PRIVATE -w)
ferrocut_ofx_bundle(ofx_invert Invert)

# Ferrocut's own test plugins (crash / abort / hang / leak / unpremult).
add_library(ferrocut_test_plugins MODULE plugins/ferrocut_test_plugins.cpp)
target_include_directories(ferrocut_test_plugins PRIVATE "${OPENFX_DIR}/include")
ferrocut_ofx_bundle(ferrocut_test_plugins FerrocutTest)
