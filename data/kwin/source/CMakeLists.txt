cmake_minimum_required(VERSION 3.22)
project(lg_buddy_kwin_inhibition LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 23)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
set(QT_MAJOR_VERSION 6)

find_package(ECM REQUIRED NO_MODULE)
list(APPEND CMAKE_MODULE_PATH ${ECM_MODULE_PATH})
include(KDEInstallDirs6)
find_package(Qt6 REQUIRED COMPONENTS Core DBus Widgets)
find_package(KWin REQUIRED CONFIG)

add_library(lg_buddy_inhibition MODULE main.cpp)
set_target_properties(lg_buddy_inhibition PROPERTIES PREFIX "")
target_link_libraries(lg_buddy_inhibition PRIVATE KWin::kwin Qt6::DBus)
install(TARGETS lg_buddy_inhibition DESTINATION ${KDE_INSTALL_PLUGINDIR}/kwin/plugins)

if(NOT LG_BUDDY_KWIN_BUILD_ID MATCHES "^[0-9a-f]+$")
    message(FATAL_ERROR "LG_BUDDY_KWIN_BUILD_ID is required; use data/kwin/build.sh")
endif()
target_compile_definitions(lg_buddy_inhibition PRIVATE LG_BUDDY_KWIN_BUILD_ID="${LG_BUDDY_KWIN_BUILD_ID}")
file(WRITE "${CMAKE_BINARY_DIR}/build-info.tsv" "${KWin_VERSION}\t${Qt6Core_VERSION}\t${CMAKE_SYSTEM_PROCESSOR}\t${LG_BUDDY_KWIN_BUILD_ID}\n")
