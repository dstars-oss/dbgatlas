cmake_minimum_required(VERSION 3.24)

if(POLICY CMP0091)
    cmake_policy(SET CMP0091 NEW)
endif()

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

add_library(dbgatlas_etw SHARED
    adapters/etw/dbgatlas_etw.cpp
)

add_library(dbgatlas_ida SHARED
    adapters/ida/dbgatlas_ida.cpp
    adapters/ida/dbgatlas_ida_runtime.cpp
)

if(WIN32)
    set_target_properties(dbgatlas_dbgeng PROPERTIES PREFIX "")
    set_target_properties(dbgatlas_etw PROPERTIES PREFIX "")
    set_target_properties(dbgatlas_ida PROPERTIES PREFIX "")
endif()

target_compile_definitions(dbgatlas_dbgeng PRIVATE DBGATLAS_NATIVE_BUILD)
target_include_directories(dbgatlas_dbgeng
    PUBLIC
        "${CMAKE_CURRENT_SOURCE_DIR}/include"
)

target_compile_definitions(dbgatlas_etw PRIVATE DBGATLAS_NATIVE_BUILD)
target_include_directories(dbgatlas_etw
    PUBLIC
        "${CMAKE_CURRENT_SOURCE_DIR}/include"
)

target_compile_definitions(dbgatlas_ida PRIVATE
    DBGATLAS_NATIVE_BUILD
    __IDP__
    __EA64__=1
    $<$<PLATFORM_ID:Windows>:__NT__ WIN32 _WINDOWS>
)
target_include_directories(dbgatlas_ida
    PUBLIC
        "${CMAKE_CURRENT_SOURCE_DIR}/include"
)

if(WIN32)
    target_link_libraries(dbgatlas_dbgeng PRIVATE DbgEng)
    target_link_libraries(dbgatlas_etw PRIVATE Advapi32 Tdh Ole32 OleAut32 uuid)
endif()

if(MSVC)
    set_property(TARGET dbgatlas_dbgeng PROPERTY
        MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
    )
    set_property(TARGET dbgatlas_etw PROPERTY
        MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
    )
    set_property(TARGET dbgatlas_ida PROPERTY
        MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
    )
    target_compile_options(dbgatlas_dbgeng PRIVATE /W4 /permissive-)
    target_compile_options(dbgatlas_etw PRIVATE /W4 /permissive-)
    target_compile_options(dbgatlas_ida PRIVATE /W4 /permissive-)
else()
    target_compile_options(dbgatlas_dbgeng PRIVATE -Wall -Wextra -Wpedantic)
    target_compile_options(dbgatlas_etw PRIVATE -Wall -Wextra -Wpedantic)
    target_compile_options(dbgatlas_ida PRIVATE -Wall -Wextra -Wpedantic)
endif()
