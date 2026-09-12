-module(counter).
-behaviour(gen_server).
-export([start_link/0, increment/1, value/0]).
-export([init/1, handle_call/3, handle_cast/2]).
-record(state, {count = 0 :: integer()}).

start_link() -> gen_server:start_link({local, ?MODULE}, ?MODULE, [], []).
increment(N) when is_integer(N) -> gen_server:cast(?MODULE, {increment, N}).
value() -> gen_server:call(?MODULE, value).

init([]) -> {ok, #state{}}.
handle_call(value, _From, State = #state{count = C}) -> {reply, C, State}.
handle_cast({increment, N}, State = #state{count = C}) ->
    {noreply, State#state{count = bump(C, N)}}.
bump(C, N) -> C + N.
