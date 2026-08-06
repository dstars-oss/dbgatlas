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

set(DBGATLAS_IDA_SDK_930_INCLUDE_DIR
    "${CMAKE_CURRENT_SOURCE_DIR}/../3rdpart/cpp/ida-sdk/930/include"
    CACHE PATH "Vendored IDA 9.3 SDK include directory"
)
set(DBGATLAS_IDA_SDK_940_INCLUDE_DIR
    "${CMAKE_CURRENT_SOURCE_DIR}/../3rdpart/cpp/ida-sdk/940/include"
    CACHE PATH "Vendored IDA 9.4 SDK include directory"
)
set(DBGATLAS_NLOHMANN_JSON_INCLUDE_DIR
    "${CMAKE_CURRENT_SOURCE_DIR}/../3rdpart/cpp/nlohmann-json/include"
    CACHE PATH "Vendored nlohmann/json include directory"
)
foreach(IDA_SDK_VERSION IN ITEMS 930 940)
    if(NOT EXISTS "${DBGATLAS_IDA_SDK_${IDA_SDK_VERSION}_INCLUDE_DIR}/pro.h")
        message(FATAL_ERROR
            "IDA SDK ${IDA_SDK_VERSION} headers are required at "
            "${DBGATLAS_IDA_SDK_${IDA_SDK_VERSION}_INCLUDE_DIR}"
        )
    endif()
endforeach()
if(NOT EXISTS "${DBGATLAS_NLOHMANN_JSON_INCLUDE_DIR}/nlohmann/json.hpp")
    message(FATAL_ERROR
        "nlohmann/json single header is required at "
        "${DBGATLAS_NLOHMANN_JSON_INCLUDE_DIR}/nlohmann/json.hpp"
    )
endif()

add_library(dbgatlas_dbgeng SHARED
    adapters/dbgeng/dbgatlas_dbgeng.cpp
)

add_library(dbgatlas_etw SHARED
    adapters/etw/dbgatlas_etw.cpp
)

function(dbgatlas_add_ida_provider target_name sdk_version ida_minor sdk_include_dir)
    add_library(${target_name} SHARED
        adapters/ida/dbgatlas_ida.cpp
        adapters/ida/dbgatlas_ida_runtime.cpp
    )
    set_target_properties(${target_name} PROPERTIES
        PREFIX ""
        OUTPUT_NAME "${target_name}"
    )
    target_compile_definitions(${target_name} PRIVATE
        DBGATLAS_NATIVE_BUILD
        DBGATLAS_IDA_EXPECTED_SDK_VERSION=${sdk_version}
        DBGATLAS_IDA_RUNTIME_MAJOR=9
        DBGATLAS_IDA_RUNTIME_MINOR=${ida_minor}
        __IDP__
        __EA64__=1
        $<$<PLATFORM_ID:Windows>:__NT__ WIN32 _WINDOWS>
    )
    target_include_directories(${target_name}
        PUBLIC
            "${CMAKE_CURRENT_SOURCE_DIR}/include"
        SYSTEM PRIVATE
            "${sdk_include_dir}"
            "${DBGATLAS_NLOHMANN_JSON_INCLUDE_DIR}"
    )
    if(MSVC)
        set_property(TARGET ${target_name} PROPERTY
            MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
        )
        target_compile_options(${target_name} PRIVATE /W4 /permissive-)
    else()
        target_compile_options(${target_name} PRIVATE -Wall -Wextra -Wpedantic)
    endif()
endfunction()

dbgatlas_add_ida_provider(
    dbgatlas_ida_sdk_930
    930
    3
    "${DBGATLAS_IDA_SDK_930_INCLUDE_DIR}"
)
dbgatlas_add_ida_provider(
    dbgatlas_ida_sdk_940
    940
    4
    "${DBGATLAS_IDA_SDK_940_INCLUDE_DIR}"
)
add_custom_target(dbgatlas_ida_providers
    DEPENDS dbgatlas_ida_sdk_930 dbgatlas_ida_sdk_940
)

if(WIN32)
    set_target_properties(dbgatlas_dbgeng PROPERTIES PREFIX "")
    set_target_properties(dbgatlas_etw PROPERTIES PREFIX "")
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

if(WIN32)
    target_link_libraries(dbgatlas_etw PRIVATE Advapi32 Tdh Ole32 OleAut32 uuid)
endif()

if(MSVC)
    set_property(TARGET dbgatlas_dbgeng PROPERTY
        MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
    )
    set_property(TARGET dbgatlas_etw PROPERTY
        MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>"
    )
    target_compile_options(dbgatlas_dbgeng PRIVATE /W4 /permissive-)
    target_compile_options(dbgatlas_etw PRIVATE /W4 /permissive-)
else()
    target_compile_options(dbgatlas_dbgeng PRIVATE -Wall -Wextra -Wpedantic)
    target_compile_options(dbgatlas_etw PRIVATE -Wall -Wextra -Wpedantic)
endif()
