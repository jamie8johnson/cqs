-module(calculator).
-behaviour(gen_server).
-export([start_link/0, add/2, subtract/2]).

-type result() :: {ok, number()} | {error, term()}.

-record(state, {
    count = 0 :: non_neg_integer(),
    name :: string()
}).

-callback init(Args :: term()) -> {ok, term()}.

-spec add(number(), number()) -> number().
add(A, B) ->
    A + B.

-spec subtract(number(), number()) -> number().
subtract(A, B) ->
    A - B.

start_link() ->
    gen_server:start_link({local, ?MODULE}, ?MODULE, [], []).

init([]) ->
    {ok, #state{count = 0, name = "calc"}}.

handle_call({add, A, B}, _From, State) ->
    Result = add(A, B),
    {reply, Result, State#state{count = State#state.count + 1}}.

process(Data) ->
    Trimmed = string:trim(Data),
    io:format("~s~n", [Trimmed]),
    helper(Trimmed).

helper(X) -> X.
