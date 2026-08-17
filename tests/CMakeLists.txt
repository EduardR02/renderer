add_executable(sr_tests unit_tests.cpp)
target_link_libraries(sr_tests PRIVATE sr_core)
add_test(NAME sr_tests COMMAND sr_tests)
set_tests_properties(sr_tests PROPERTIES TIMEOUT 30)
