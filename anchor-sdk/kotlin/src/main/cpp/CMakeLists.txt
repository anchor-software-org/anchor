cmake_minimum_required(VERSION 3.22)
project(anchor_msquic_jni C)

if(NOT ANDROID)
  message(FATAL_ERROR "The Anchor MsQuic bridge is built only for Android")
endif()

if(NOT DEFINED ANDROID_ABI)
  message(FATAL_ERROR "ANDROID_ABI is required")
endif()

set(msquic_library "${CMAKE_CURRENT_LIST_DIR}/../jniLibs/${ANDROID_ABI}/libmsquic.so")
if(NOT EXISTS "${msquic_library}")
  message(FATAL_ERROR "Build MsQuic first: missing ${msquic_library}")
endif()

set(msquic_include_dir "${CMAKE_CURRENT_LIST_DIR}/third_party/msquic/src/inc")
set(jni_include_dir "${ANDROID_NDK}/toolchains/llvm/prebuilt/linux-x86_64/sysroot/usr/include")
if(NOT EXISTS "${msquic_include_dir}/msquic.h" OR NOT EXISTS "${jni_include_dir}/jni.h")
  message(FATAL_ERROR "MsQuic or NDK JNI headers are unavailable")
endif()

add_library(msquic SHARED IMPORTED)
set_target_properties(msquic PROPERTIES IMPORTED_LOCATION "${msquic_library}")

add_library(anchor_msquic_jni SHARED anchor_msquic_jni.c)
target_include_directories(anchor_msquic_jni PRIVATE "${msquic_include_dir}" "${jni_include_dir}")
target_link_libraries(anchor_msquic_jni PRIVATE msquic log)
