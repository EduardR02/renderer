cmake_minimum_required(VERSION 3.20)
project(SpotifyRenderer VERSION 1.0.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)

if(MSVC)
  add_compile_options(/W4 /permissive- /EHsc /utf-8 /Zc:__cplusplus /Zc:preprocessor)
endif()

add_library(sr_core STATIC
  src/util.cpp
  src/app_paths.cpp
  src/log.cpp
  src/settings.cpp
  src/secure_store.cpp
  src/playback_engine_client.cpp
  src/http.cpp
  src/oauth.cpp
  src/spotify_api.cpp
  src/tray.cpp
  src/ui_main.cpp
  src/app.cpp
)

target_include_directories(sr_core PUBLIC
  ${CMAKE_CURRENT_SOURCE_DIR}/third_party
  ${CMAKE_CURRENT_SOURCE_DIR}/src
)
target_compile_definitions(sr_core PUBLIC
  WIN32_LEAN_AND_MEAN
  NOMINMAX
  UNICODE
  _UNICODE
)
target_link_libraries(sr_core PUBLIC
  user32 gdi32 shell32 winhttp crypt32 bcrypt advapi32 gdiplus ole32
  comctl32 uxtheme ws2_32
)

add_executable(SpotifyRenderer WIN32 src/main.cpp)
target_link_libraries(SpotifyRenderer PRIVATE sr_core)
set_target_properties(SpotifyRenderer PROPERTIES OUTPUT_NAME "SpotifyRenderer")

set(CARGO_EXECUTABLE cargo CACHE FILEPATH "Path to the Rust cargo executable")
set(PLAYBACK_ENGINE_MANIFEST "${CMAKE_CURRENT_SOURCE_DIR}/engine/Cargo.toml")
set(PLAYBACK_ENGINE_LOCK "${CMAKE_CURRENT_SOURCE_DIR}/engine/Cargo.lock")
set(PLAYBACK_ENGINE_TARGET_DIR "${CMAKE_CURRENT_BINARY_DIR}/engine-target")
set(PLAYBACK_ENGINE_EXE
    "${PLAYBACK_ENGINE_TARGET_DIR}/release/SpotifyPlaybackEngine.exe")
file(GLOB_RECURSE PLAYBACK_ENGINE_SOURCES CONFIGURE_DEPENDS
     "${CMAKE_CURRENT_SOURCE_DIR}/engine/src/*.rs")
add_custom_command(
  OUTPUT "${PLAYBACK_ENGINE_EXE}"
  COMMAND ${CMAKE_COMMAND} -E env
          "CARGO_TARGET_DIR=${PLAYBACK_ENGINE_TARGET_DIR}"
          "${CARGO_EXECUTABLE}" build --release --locked --manifest-path
          "${PLAYBACK_ENGINE_MANIFEST}"
  DEPENDS "${PLAYBACK_ENGINE_MANIFEST}" "${PLAYBACK_ENGINE_LOCK}"
          ${PLAYBACK_ENGINE_SOURCES}
  WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/engine"
  COMMENT "Building standalone SpotifyPlaybackEngine"
  VERBATIM
)
add_custom_target(SpotifyPlaybackEngine ALL DEPENDS "${PLAYBACK_ENGINE_EXE}")

add_custom_target(package
  COMMAND ${CMAKE_COMMAND} -E make_directory ${CMAKE_SOURCE_DIR}/dist
  COMMAND ${CMAKE_COMMAND} -E copy_if_different
          $<TARGET_FILE:SpotifyRenderer> ${CMAKE_SOURCE_DIR}/dist/
  COMMAND ${CMAKE_COMMAND} -E copy_if_different
          "${PLAYBACK_ENGINE_EXE}" ${CMAKE_SOURCE_DIR}/dist/
  COMMAND ${CMAKE_COMMAND} -E copy_if_different
          ${CMAKE_SOURCE_DIR}/README.md ${CMAKE_SOURCE_DIR}/dist/
  DEPENDS SpotifyRenderer SpotifyPlaybackEngine
  COMMENT "Packaging SpotifyRenderer and SpotifyPlaybackEngine into dist/"
)

include(CTest)
if(BUILD_TESTING)
  add_subdirectory(tests)
endif()
