cmake_minimum_required(VERSION 3.24)

project(DbgAtlasNative LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

set(CMAKE_RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin")
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib")
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/lib")

add_library(dbgatlas_dbgeng SHARED
    adapters/dbgeng/dbgatlas_dbgeng.cpp
)

target_compile_definitions(dbgatlas_dbgeng PRIVATE DBGATLAS_NATIVE_BUILD)
target_include_directories(dbgatlas_dbgeng
    PUBLIC
        "${CMAKE_CURRENT_SOURCE_DIR}/include"
)

if(MSVC)
    target_compile_options(dbgatlas_dbgeng PRIVATE /W4 /permissive-)
else()
    target_compile_options(dbgatlas_dbgeng PRIVATE -Wall -Wextra -Wpedantic)
endif()
