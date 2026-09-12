-module(counter_tests).
-include_lib("eunit/include/eunit.hrl").
increment_test() ->
    {ok, _} = counter:start_link(),
    ok = counter:increment(2),
    ?assertEqual(2, counter:value()).
