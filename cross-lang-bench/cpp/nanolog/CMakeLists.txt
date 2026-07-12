cmake_minimum_required(VERSION 3.20)
project(nanolog_harness LANGUAGES CXX)

# NanoLog requires C++17 and is Linux-only.
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_CXX_FLAGS_RELEASE "-O3 -DNDEBUG")
set(CMAKE_CXX_FLAGS_DEBUG "-O0 -g")

# setup.sh fetches NanoLog into ../NanoLog/ and builds the runtime via
# `make -C ../NanoLog/runtime/`. The runner passes -DNANOLOG_RUNTIME_DIR
# pointing at the built runtime.
if(NOT DEFINED NANOLOG_RUNTIME_DIR)
    set(NANOLOG_RUNTIME_DIR "${CMAKE_CURRENT_SOURCE_DIR}/../NanoLog/runtime")
endif()

# Header and library locations.
set(NANOLOG_INCLUDE_DIR "${NANOLOG_RUNTIME_DIR}")
set(NANOLOG_LIBRARY "${NANOLOG_RUNTIME_DIR}/libNanoLog.a")

if(NOT EXISTS "${NANOLOG_LIBRARY}")
    message(FATAL_ERROR
        "NanoLog runtime not found at ${NANOLOG_LIBRARY}. "
        "Run setup.sh first or pass -DNANOLOG_RUNTIME_DIR=<path>")
endif()

add_executable(nanolog_harness main.cpp)
target_include_directories(nanolog_harness PRIVATE "${NANOLOG_INCLUDE_DIR}")
target_link_directories(nanolog_harness PRIVATE "${NANOLOG_RUNTIME_DIR}")
target_link_libraries(nanolog_harness PRIVATE NanoLog rt pthread)
target_compile_options(nanolog_harness PRIVATE -O3 -Werror=format)
