cmake_minimum_required(VERSION 3.20)
project(quill_harness LANGUAGES CXX)

# Quill requires C++17.
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

# Optimise for the host CPU (same as -march=native for the bench).
set(CMAKE_CXX_FLAGS_RELEASE "-O3 -DNDEBUG")
set(CMAKE_CXX_FLAGS_DEBUG "-O0 -g")

# Fetch Quill at a pinned version.
include(FetchContent)
FetchContent_Declare(
    quill
    GIT_REPOSITORY  https://github.com/odygrd/quill.git
    GIT_TAG         v7.5.0
    GIT_SHALLOW     TRUE
)
FetchContent_MakeAvailable(quill)

add_executable(quill_harness main.cpp)
target_link_libraries(quill_harness PRIVATE quill::quill)
target_compile_options(quill_harness PRIVATE -O3)
