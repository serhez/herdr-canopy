//! The Lua scripts Canopy runs in Rex, under Lua 5.1 (Rex's version) against a
//! stubbed `rex` table that records calls and answers from canned results.

use mlua::{Lua, LuaSerdeExt};
use serde_json::{Value, json};

const AGENT: &str = include_str!("scripts/agent.lua");

/// `T.answers[method]` answers `rex.call`: an `err` field fails the call with
/// that message, and a method without an answer fails as an internal error.
/// `rex.wait` returns the first of `T.events` its filter accepts.
const STUB: &str = r#"
T = { calls = {}, waits = {} }
rex = {
  call = function(method, args)
    table.insert(T.calls, { method = method, args = args })
    local answer = T.answers[method]
    if answer == nil then return nil, "stream response failed (status=500 code=internal)" end
    if answer.err then return nil, answer.err end
    return answer
  end,
  wait = function(event, filter, timeout)
    table.insert(T.waits, { event = event, timeout = timeout })
    for _, e in ipairs(T.events) do
      if e.name == event and filter(e.event, e.ctx) then return e.event, e.ctx end
    end
    return nil, "timeout"
  end,
  json = { encode = __encode },
}
print = function(text) T.printed = text end
"#;

/// What a script run did.
struct Run {
    /// Each `rex.call`: method and arguments.
    calls: Vec<(String, Value)>,
    /// Each `rex.wait`: event and timeout.
    waits: Vec<(String, f64)>,
    /// What it printed, or its error.
    outcome: Result<Value, String>,
}

impl Run {
    fn methods(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(|(method, _)| method.as_str())
            .collect()
    }

    fn sent(&self) -> Vec<Value> {
        self.calls
            .iter()
            .filter(|(method, _)| method == "block.event")
            .map(|(_, args)| args.clone())
            .collect()
    }
}

fn execute(script: &str, args: Value, answers: Value, events: Value) -> Run {
    let lua = Lua::new();
    let encode = lua
        .create_function(|lua, value: mlua::Value| Ok(lua.from_value::<Value>(value)?.to_string()))
        .unwrap();
    lua.globals().set("__encode", encode).unwrap();
    lua.load(STUB).exec().unwrap();
    let t: mlua::Table = lua.globals().get("T").unwrap();
    t.set("answers", lua.to_value(&answers).unwrap()).unwrap();
    t.set("events", lua.to_value(&events).unwrap()).unwrap();
    let rex: mlua::Table = lua.globals().get("rex").unwrap();
    rex.set("args", lua.to_value(&args).unwrap()).unwrap();
    let ran = lua.load(script).exec();
    let recorded = |key: &str| floats(lua.from_value(t.get::<mlua::Value>(key).unwrap()).unwrap());
    let calls = recorded("calls")
        .as_array()
        .map(|calls| {
            calls
                .iter()
                .map(|c| (c["method"].as_str().unwrap().to_owned(), c["args"].clone()))
                .collect()
        })
        .unwrap_or_default();
    let waits = recorded("waits")
        .as_array()
        .map(|waits| {
            waits
                .iter()
                .map(|w| {
                    (
                        w["event"].as_str().unwrap().to_owned(),
                        w["timeout"].as_f64().unwrap(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let outcome = ran.map_err(|error| error.to_string()).map(|()| {
        let printed: Option<String> = t.get("printed").unwrap();
        floats(serde_json::from_str(&printed.expect("the script printed nothing")).unwrap())
    });
    Run {
        calls,
        waits,
        outcome,
    }
}

/// `value` with every number a float, as Lua 5.1 has them.
fn floats(value: Value) -> Value {
    match value {
        Value::Number(n) => json!(n.as_f64().unwrap()),
        Value::Array(items) => Value::Array(items.into_iter().map(floats).collect()),
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, floats(v))).collect()),
        other => other,
    }
}

const PROCESS: &str = "com.superlogical.terminal.process";
const STATUS_CHANGED: &str = "com.superlogical.terminal.program_status_changed";

fn look(submit: Option<Value>, wait_s: f64) -> Value {
    let mut args =
        json!({"session_id": "session:pad", "block_id": "block:claude", "wait_s": wait_s});
    if let Some(submit) = submit {
        args["submit"] = submit;
    }
    args
}

fn submit(pid: u32, at_prompt: bool) -> Option<Value> {
    Some(json!({"text": "Review the diff", "pid": pid, "at_prompt": at_prompt}))
}

/// Rex's answers for block `block:claude`: Claude (pid 30) in front of zsh (pid 3).
fn answers() -> Value {
    json!({
        "session.attach": {"client_id": "client:script"},
        PROCESS: {"foreground": {"name": "claude", "pid": 30}, "ancestors": [{"name": "zsh", "pid": 3}]},
        "com.superlogical.terminal.title": {"title": "✳ Reviewing"},
        "com.superlogical.terminal.program_status": {"records": [{"state": "idle", "app": "claude-code"}]},
        "block.event": {},
    })
}

fn status_changed(block: &str, state: &str) -> Value {
    json!({"name": STATUS_CHANGED, "event": {"id": "1", "record": {"state": state}},
           "ctx": {"session_id": "session:pad", "block_id": block}})
}

#[test]
fn a_submission_pastes_the_text_then_presses_enter_in_the_agents_block() {
    let run = execute(AGENT, look(submit(30, false), 0.0), answers(), json!([]));
    assert_eq!(
        run.methods(),
        [
            "session.attach",
            PROCESS,
            "block.event",
            "block.event",
            "block.event",
            PROCESS,
            "com.superlogical.terminal.title",
            "com.superlogical.terminal.program_status",
        ]
    );
    assert_eq!(run.calls[0].1, json!({"session_id": "session:pad"}));
    let enter = |action: &str| {
        json!({"type": "key", "payload": {"key": "enter", "mods": "", "consumed_mods": "", "text": "",
                                          "unshifted_codepoint": 0, "composing": false, "action": action}})
    };
    let events = [
        json!({"type": "paste", "payload": {"text": "Review the diff"}}),
        enter("press"),
        enter("release"),
    ];
    let expected: Vec<Value> = events
        .into_iter()
        .map(|event| {
            floats(json!({"session_id": "session:pad", "block_id": "block:claude", "event": event}))
        })
        .collect();
    assert_eq!(run.sent(), expected);
    assert!(run.waits.is_empty(), "no wait without wait time");
    let printed = run.outcome.unwrap();
    assert_eq!(printed["process"], floats(answers()[PROCESS].clone()));
    assert_eq!(printed["title"]["title"], "✳ Reviewing");
    assert_eq!(printed["status"]["records"][0]["state"], "idle");
    assert_eq!(printed["active"], false);
}

#[test]
fn a_submission_sends_nothing_unless_its_process_still_runs_in_the_block() {
    for (pid, at_prompt, sent) in [
        (30, false, true),
        (3, false, true),
        (30, true, true),
        (3, true, false),
        (99, false, false),
    ] {
        let run = execute(
            AGENT,
            look(submit(pid, at_prompt), 0.0),
            answers(),
            json!([]),
        );
        if sent {
            assert_eq!(run.sent().len(), 3, "pid {pid}, at prompt {at_prompt}");
        } else {
            assert_eq!(run.sent().len(), 0, "pid {pid}, at prompt {at_prompt}");
            assert_eq!(run.outcome, Ok(json!({"refused": true})));
        }
    }
    let mut exited = answers();
    exited[PROCESS] = json!({"ancestors": [{"name": "zsh", "pid": 3}]});
    let run = execute(AGENT, look(submit(3, false), 0.0), exited, json!([]));
    assert_eq!(
        (run.sent().len(), run.outcome),
        (0, Ok(json!({"refused": true})))
    );
}

#[test]
fn a_look_waits_for_the_blocks_next_status_change() {
    let elsewhere = status_changed("block:other", "working");
    for (events, active) in [
        (
            json!([elsewhere, status_changed("block:claude", "working")]),
            true,
        ),
        (json!([status_changed("block:claude", "blocked")]), true),
        (
            json!([elsewhere, status_changed("block:claude", "idle")]),
            false,
        ),
        (json!([elsewhere]), false),
        (json!([]), false),
    ] {
        let run = execute(AGENT, look(None, 0.75), answers(), events.clone());
        assert_eq!(run.waits, [(STATUS_CHANGED.to_owned(), 0.75)], "{events}");
        assert!(run.sent().is_empty(), "a look sends nothing");
        assert_eq!(run.outcome.unwrap()["active"], active, "{events}");
    }
    let run = execute(
        AGENT,
        look(submit(30, false), 0.5),
        answers(),
        json!([status_changed("block:claude", "working")]),
    );
    assert_eq!(run.waits.len(), 1);
    assert_eq!(
        run.outcome.unwrap()["active"],
        true,
        "a submission's look sees the agent react"
    );
}

#[test]
fn a_closed_block_or_session_is_gone_and_other_failures_are_errors() {
    // Rex's errors for a closed block and a killed session.
    let closed_block = r#"stream response "control-9" failed: block not found: block:claude (status=404 code=not_found)"#;
    let killed_session = r#"stream response "control-6" failed: session session:pad not found (status=404 code=session_not_found)"#;
    let mut closed = answers();
    closed[PROCESS] = json!({"err": closed_block});
    for submission in [None, submit(30, false)] {
        let run = execute(AGENT, look(submission, 0.0), closed.clone(), json!([]));
        assert_eq!(
            (run.sent().len(), run.outcome),
            (0, Ok(json!({"gone": true})))
        );
    }
    let mut ended = answers();
    ended["session.attach"] = json!({"err": killed_session});
    let run = execute(AGENT, look(submit(30, false), 0.0), ended, json!([]));
    assert_eq!(
        (run.sent().len(), run.outcome),
        (0, Ok(json!({"gone": true})))
    );
    let mut failing = answers();
    failing[PROCESS] = json!({"err": "server is shutting down (status=503 code=unavailable)"});
    let error = execute(AGENT, look(None, 0.0), failing, json!([]))
        .outcome
        .unwrap_err();
    assert!(error.contains("server is shutting down"), "{error}");
    let mut refused = answers();
    refused["block.event"] =
        json!({"err": "stream is not attached to session (status=409 code=conflict)"});
    let run = execute(AGENT, look(submit(30, false), 0.0), refused, json!([]));
    assert_eq!(run.sent().len(), 1, "no Enter after a failed paste");
    assert!(run.outcome.unwrap_err().contains("not attached"));
}
