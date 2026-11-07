// SPDX-License-Identifier: MPL-2.0

use super::*;
#[test]
fn catalog_uses_first_slash_preserves_nested_ids_and_rejects_noise() {
    let catalog =
        parse_catalog(b"openrouter/deepseek/deepseek-chat\ndeepseek/deepseek-chat\n").unwrap();
    assert_eq!(catalog.models().len(), 2);
    assert_eq!(catalog.models()[1].group.as_str(), "openrouter");
    assert!(parse_catalog(b"debug configuration leaked\nprovider/model\n").is_err());
    assert!(parse_catalog(b"provider/\n").is_err());
    assert!(parse_catalog(b"provider/model\x1b[31m").is_err());
    assert!(!format!("{catalog:?}").contains("deepseek"));
}
/// The shape `opencode models --verbose` printed on 1.19.5: the id line,
/// then a pretty-printed object, the last one ending in CRLF.
#[test]
fn the_verbose_catalog_keeps_each_models_own_name() {
    let output = "opencode/beam-free\n{\n  \"id\": \"beam-free\",\n  \"providerID\": \"opencode\",\n  \"name\": \"Beam Free\",\n  \"status\": \"active\",\n  \"variants\": {\"low\": {\"reasoningEffort\": \"low\"}}\n}\nopenrouter/deepseek/deepseek-chat\n{\n  \"name\": \"   \"\n}\r\n";
    let catalog = parse_verbose_catalog(output.as_bytes()).unwrap();
    let models = catalog.models();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id.as_str(), "opencode/beam-free");
    assert_eq!(models[0].name.as_ref().unwrap().as_str(), "Beam Free");
    assert_eq!(models[1].group.as_str(), "openrouter");
    assert!(
        models[1].name.is_none(),
        "a blank name is no name; the picker formats the id"
    );
}

#[test]
fn a_malformed_verbose_catalog_is_refused_whole_so_the_plain_one_is_used() {
    for output in [
        "opencode/beam-free\n{\"name\": \"Beam Free\"\n",
        "opencode/beam-free\n[\"not an object\"]\n",
        "opencode/beam-free\n",
        "debug configuration leaked\n{}\n",
        "opencode/beam-free\n{} trailing\n",
    ] {
        assert!(
            parse_verbose_catalog(output.as_bytes()).is_err(),
            "{output:?}"
        );
    }
    // The plain listing parses what the verbose one refused.
    assert!(parse_catalog(b"opencode/beam-free\n").is_ok());
    assert_eq!(VERBOSE_CATALOG_OUTPUT_BYTES, 4 * 1024 * 1024);
}

#[test]
fn config_models_keep_the_agents_names_descriptions_and_group_names() {
    let long = "x".repeat(koru_session_protocol::MAX_MODEL_NAME_BYTES + 1);
    let flat = json!({"configOptions":[{"id":"model","type":"select","currentValue":"default",
        "options":[{"value":"default","name":"Default (recommended)","description":"Opus 4.7 · Most capable"},
                   {"value":"sonnet","name":long},
                   {"value":"haiku","name":"Haiku\u{1b}[31m","description":"  "}]}]});
    let models = config_models(&flat, "claude");
    assert_eq!(
        models[0].name.as_ref().unwrap().as_str(),
        "Default (recommended)"
    );
    assert_eq!(
        models[0].description.as_ref().unwrap().as_str(),
        "Opus 4.7 · Most capable"
    );
    assert!(
        models[0].group_name.is_none(),
        "a flat list has no group name"
    );
    assert!(models[1].name.is_none(), "a name past the bound is dropped");
    assert!(models[2].name.is_none() && models[2].description.is_none());
    assert!(ProviderModels::try_from(models).is_ok());

    let grouped = json!({"configOptions":[{"id":"model","type":"select","currentValue":"deepseek/chat",
        "options":[{"group":"deepseek","name":"DeepSeek","options":[{"value":"deepseek/chat","name":"Chat"}]}]}]});
    let models = config_models(&grouped, "opencode");
    assert_eq!(models[0].group.as_str(), "deepseek");
    assert_eq!(models[0].group_name.as_ref().unwrap().as_str(), "DeepSeek");
    assert_eq!(models[0].name.as_ref().unwrap().as_str(), "Chat");
}

#[test]
fn selection_requires_advertised_value_and_confirmed_current_value() {
    let requested = "deepseek/chat".parse().unwrap();
    let advertised = json!({"configOptions":[{"id":"model","type":"select","currentValue":"other/model",
        "options":[{"group":"deepseek","options":[{"value":"deepseek/chat","name":"Chat"}]}]}]});
    assert!(validate_selection(&advertised, &requested).is_ok());
    assert!(validate_applied(&advertised, &requested).is_err());
    assert!(validate_selection(&advertised, &"missing/model".parse().unwrap()).is_err());
    let applied = json!({"configOptions":[{"id":"model","type":"select","currentValue":"deepseek/chat","options":[]}]});
    assert!(validate_applied(&applied, &requested).is_ok());
}

#[test]
fn config_models_keep_flat_values_under_the_kind_and_grouped_ones_under_their_group() {
    let flat = json!({"configOptions":[{"id":"model","type":"select","currentValue":"opus",
        "options":[{"value":"opus","name":"Opus"},{"value":"sonnet","name":"Sonnet"},{"value":"opus","name":"again"},{"value":"has space","name":"bad"}]}]});
    let models = config_models(&flat, "claude");
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["opus", "sonnet"],
        "a repeat and an unnameable value are left out"
    );
    assert!(models.iter().all(|model| model.group.as_str() == "claude"));

    let grouped = json!({"configOptions":[{"id":"model","type":"select","currentValue":"deepseek/chat",
        "options":[{"group":"deepseek","name":"DeepSeek","options":[{"value":"deepseek/chat","name":"Chat"}]},
                   {"group":"openai","name":"OpenAI","options":[{"value":"gpt-5","name":"GPT-5"}]}]}]});
    let models = config_models(&grouped, "opencode");
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].group.as_str(), "deepseek");
    assert_eq!(models[1].group.as_str(), "openai");

    assert!(config_models(&json!({"configOptions":[]}), "kimi").is_empty());
    assert!(
        config_models(
            &json!({"configOptions":[{"id":"mode","type":"select","options":[]}]}),
            "kimi"
        )
        .is_empty()
    );
}

struct SelectionPeer {
    selected: Option<String>,
    offer: bool,
    refuse: bool,
    sent: Arc<std::sync::Mutex<Vec<Value>>>,
    incoming: VecDeque<Value>,
}
impl JsonRpcTransport for SelectionPeer {
    fn send(&mut self, message: &Value) -> Result<(), ProviderFailure> {
        self.sent.lock().unwrap().push(message.clone());
        let result = match message["method"].as_str().unwrap() {
            "initialize" => {
                json!({"protocolVersion":1,"agentCapabilities":{"mcpCapabilities":{"http":true}},"authMethods":[]})
            }
            "session/new" => json!({"sessionId":"private-session-sentinel","configOptions":[{
                "id":"model","type":"select","name":"Model","currentValue":"provider/default",
                "options":if self.offer { json!([{"value":"private-provider/private-model-sentinel","name":"Private model"}]) } else { json!([]) }
            }]}),
            "session/set_config_option" => {
                if self.refuse {
                    self.incoming.push_back(json!({"jsonrpc":"2.0","id":message["id"],"error":{"code":-32602,"message":"private-error-sentinel"}}));
                    return Ok(());
                }
                self.selected = message["params"]["value"].as_str().map(str::to_owned);
                json!({"configOptions":[{"id":"model","type":"select","currentValue":self.selected,"options":[]}]})
            }
            "session/prompt" => json!({"stopReason":"end_turn"}),
            _ => panic!("unexpected method"),
        };
        self.incoming
            .push_back(json!({"jsonrpc":"2.0","id":message["id"],"result":result}));
        Ok(())
    }
    fn receive(&mut self, _: Duration) -> Result<TransportReceive, ProviderFailure> {
        Ok(self
            .incoming
            .pop_front()
            .map_or(TransportReceive::Timeout, TransportReceive::Frame))
    }
    fn shutdown(&mut self, _: Duration) -> Result<(), ProviderFailure> {
        Ok(())
    }
}
pub(in crate::providers::acp) fn selected_control(
    offer: bool,
    refuse: bool,
) -> (AcpControl, Arc<std::sync::Mutex<Vec<Value>>>) {
    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut config = AcpConfig::new(
        AcpRoute::opencode(),
        "fixture-opencode",
        // Only validated, never opened — but absolute on the host, and a drive
        // letter is not on a Mac.
        PathBuf::from(if cfg!(windows) {
            "C:/project"
        } else {
            "/project"
        }),
        "1.18.27".parse().unwrap(),
        Arc::new(crate::NoopAttachmentPathResolver),
    )
    .unwrap();
    config.delegation = Some(koru_session_protocol::DelegationLaunch {
        model: Some("private-provider/private-model-sentinel".parse().unwrap()),
        mcp_endpoint: "http://localhost:1/mcp".parse().unwrap(),
        credential: None,
        tools: Default::default(),
        resume: None,
    });
    (
        AcpControl::with_transport(
            config,
            Some(Box::new(SelectionPeer {
                selected: None,
                offer,
                refuse,
                sent: sent.clone(),
                incoming: VecDeque::new(),
            })),
        ),
        sent,
    )
}
#[test]
fn model_is_applied_before_prompt_and_never_claimed_as_reported() {
    let (mut control, sent) = selected_control(true, false);
    let capabilities = control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap();
    assert_eq!(
        capabilities
            .delegation
            .economics
            .model
            .requested
            .as_ref()
            .unwrap()
            .as_str(),
        "private-provider/private-model-sentinel"
    );
    assert!(capabilities.delegation.economics.model.reported.is_none());
    control
        .submit(AgentInput {
            model: None,
            turn_id: AgentTurnId::mint(),
            text: "private-prompt-sentinel".into(),
            attachments: vec![],
            references: Vec::new(),
            mode: None,
            fast: None,
            reasoning: None,
            skills: Vec::new(),
        })
        .unwrap();
    let frames = sent.lock().unwrap();
    let methods: Vec<_> = frames
        .iter()
        .map(|frame| frame["method"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "session/new",
            "session/set_config_option",
            "session/prompt"
        ]
    );
}
#[test]
fn missing_or_refused_model_fails_before_any_prompt() {
    for (offer, refuse) in [(false, false), (true, true)] {
        let (mut control, sent) = selected_control(offer, refuse);
        let error = control
            .start(AgentStart::fresh(SessionId::mint(), None))
            .unwrap_err();
        assert!(!format!("{error:?}").contains("private"));
        assert!(
            !sent
                .lock()
                .unwrap()
                .iter()
                .any(|frame| frame["method"] == "session/prompt")
        );
        control.shutdown().unwrap();
    }
}

#[test]
fn a_stdio_bridge_rides_session_new_as_acps_untagged_server() {
    let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
    let config = AcpConfig::new(
        AcpRoute::kimi(),
        "fixture-kimi",
        PathBuf::from(if cfg!(windows) {
            "C:/project"
        } else {
            "/project"
        }),
        "0.42.0".parse().unwrap(),
        Arc::new(crate::NoopAttachmentPathResolver),
    )
    .unwrap()
    .with_mcp_servers(vec![crate::providers::McpServerLaunch {
        name: "koru".into(),
        command: "C:/koru/koru-mcp.exe".into(),
        args: vec!["serve".into(), "--stdio".into()],
        env: Vec::new(),
    }]);
    let mut control = AcpControl::with_transport(
        config,
        Some(Box::new(SelectionPeer {
            selected: None,
            offer: false,
            refuse: false,
            sent: sent.clone(),
            incoming: VecDeque::new(),
        })),
    );
    control
        .start(AgentStart::fresh(SessionId::mint(), None))
        .unwrap();
    let sent = sent.lock().unwrap();
    let new_session = sent
        .iter()
        .find(|message| message["method"] == "session/new")
        .expect("session/new sent");
    let servers = new_session["params"]["mcpServers"].as_array().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["name"], "koru");
    assert_eq!(servers[0]["command"], "C:/koru/koru-mcp.exe");
    assert_eq!(servers[0]["args"], json!(["serve", "--stdio"]));
    assert!(
        servers[0].get("type").is_none(),
        "stdio is the untagged shape"
    );
}

/// The shape claude-agent-acp 0.76.0 answers with (`session-effort.js`):
/// an `effort` select in the `thought_level` category, its values the
/// current model's levels plus `default`.
fn claude_options(current_model: &str, effort: Option<(&str, &[&str])>) -> Value {
    let mut options = vec![json!({"id":"model","type":"select","category":"model",
        "currentValue":current_model,
        "options":[{"value":"default","name":"Default (recommended)"},
                   {"value":"opus","name":"Opus"},{"value":"haiku","name":"Haiku"}]})];
    if let Some((current, values)) = effort {
        options.push(json!({"id":"effort","type":"select","category":"thought_level",
            "currentValue":current,
            "options":values.iter().map(|value| json!({"value":value,"name":value})).collect::<Vec<_>>()}));
    }
    json!({"configOptions":options})
}

/// **D15: the session's `thought_level` option is its current model's
/// levels**, in Koru's words, with the agent's own spelling kept for
/// applying one; the other models carry none until they are walked.
#[test]
fn the_thought_level_option_gives_the_current_models_levels() {
    let session = claude_options(
        "default",
        Some((
            "high",
            &["default", "low", "medium", "high", "xhigh", "max", "ultra"],
        )),
    );
    let level = thought_level(&session).unwrap();
    assert_eq!(level.id, "effort");
    assert_eq!(level.current.as_deref(), Some("high"));
    assert_eq!(level.dropped, 1);
    assert_eq!(level.value_for(LaunchReasoning::XHigh), Some("xhigh"));
    assert_eq!(level.value_for(LaunchReasoning::Minimal), None);
    let reasoning = level.reasoning.unwrap();
    assert_eq!(reasoning.levels.len(), 5);
    assert_eq!(reasoning.default, Some(LaunchReasoning::High));

    let models = config_models(&session, "claude");
    assert!(models[0].reasoning.is_some(), "the model the session is on");
    assert!(models[1].reasoning.is_none() && models[2].reasoning.is_none());
    // A model with no levels, and an answer with no option, give none.
    assert!(thought_level(&claude_options("haiku", None)).is_none());
    assert!(
        config_models(&claude_options("haiku", None), "claude")
            .iter()
            .all(|model| model.reasoning.is_none())
    );
}

/// OpenCode's `variants` are its levels; it states no default.
#[test]
fn the_verbose_catalog_reads_opencodes_variants_as_levels() {
    let output = "opencode/beam-free\n{\"name\":\"Beam Free\",\"variants\":{\"low\":{},\"high\":{},\"max\":{},\"turbo\":{}}}\nopencode/plain\n{\"name\":\"Plain\"}\n";
    let catalog = parse_verbose_catalog(output.as_bytes()).unwrap();
    let reasoning = catalog.models()[0].reasoning.as_ref().unwrap();
    assert_eq!(
        reasoning.levels,
        [
            LaunchReasoning::Low,
            LaunchReasoning::High,
            LaunchReasoning::Max
        ]
    );
    assert_eq!(reasoning.default, None);
    assert!(catalog.models()[1].reasoning.is_none());
}

/// **D11: legacy is the agent's own group**: an id of `legacy`, or a name
/// starting with *Legacy*, without case. Nothing else marks a model.
#[test]
fn an_acp_legacy_group_marks_its_models_and_nothing_else_does() {
    let session = json!({"configOptions":[{"id":"model","type":"select","currentValue":"new",
    "options":[
        {"group":"current","name":"Current","options":[{"value":"new","name":"New"}]},
        {"group":"legacy","name":"Older","options":[{"value":"old-a","name":"Old A"}]},
        {"group":"archive","name":"legacy models","options":[{"value":"old-b"}]},
        {"group":"claude-3","name":"Claude 3 (2024)","options":[{"value":"dated"}]}
    ]}]});
    let models = config_models(&session, "fixture");
    let legacy: Vec<(&str, bool)> = models
        .iter()
        .map(|model| (model.id.as_str(), model.legacy))
        .collect();
    assert_eq!(
        legacy,
        [
            ("new", false),
            ("old-a", true),
            ("old-b", true),
            ("dated", false)
        ],
        "a date in a name is never a mark"
    );
}

/// OpenCode's own `deprecated` status marks a model; `active` does not.
#[test]
fn an_opencode_deprecated_model_is_legacy() {
    let output = "opencode/old\n{\"name\":\"Old\",\"status\":\"deprecated\"}\nopencode/new\n{\"name\":\"New\",\"status\":\"active\"}\n";
    let catalog = parse_verbose_catalog(output.as_bytes()).unwrap();
    let marks: Vec<bool> = catalog.models().iter().map(|model| model.legacy).collect();
    assert_eq!(marks, [false, true], "sorted by id: new, then old");
}

/// **koru-ai D10: Claude's 1M siblings fold into a context choice.** The
/// wrapper's list as seen: *Default*, *Opus (1M context)*, *Fable*,
/// *Sonnet*, *Haiku*, *Opus*. `opus[1m]` becomes `opus`'s 1M context, in
/// the sibling's earlier place, under the base's own name; a sibling whose
/// base is not listed stays a model of its own; nothing else moves.
#[test]
fn claudes_one_million_siblings_fold_into_their_base_as_its_context() {
    let session = json!({"configOptions":[{"id":"model","type":"select","currentValue":"default",
    "options":[
        {"value":"default","name":"Default (recommended)","description":"Opus (1M context)"},
        {"value":"opus[1m]","name":"Opus (1M context)"},
        {"value":"fable","name":"Fable 5.1"},
        {"value":"sonnet","name":"Sonnet 5"},
        {"value":"haiku","name":"Haiku 4.5"},
        {"value":"opus","name":"Opus"},
        {"value":"sonnet-next[1m]","name":"Sonnet Next (1M context)"}
    ]}]});
    let models = super::super::claude::fold_context_siblings(config_models(&session, "claude"));
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "default",
            "opus",
            "fable",
            "sonnet",
            "haiku",
            "sonnet-next[1m]"
        ]
    );
    let opus = &models[1];
    assert_eq!(opus.name.as_ref().unwrap().as_str(), "Opus");
    let context = opus.context.as_ref().unwrap();
    assert_eq!(context.id.as_str(), "opus[1m]");
    assert_eq!(context.tokens, 1_000_000);
    assert!(models[5].context.is_none(), "no base listed: no choice");
    assert!(
        models
            .iter()
            .filter(|model| model.id.as_str() != "opus")
            .all(|model| model.context.is_none())
    );
    assert!(ProviderModels::try_from(models).is_ok());
}

/// **Claude's Fast mode option** (koru-ai D11): the `fast` select Koru gets
/// (it advertises no boolean options) and the boolean another client gets
/// are both read; the session's current model carries it in the catalog.
#[test]
fn the_fast_option_is_read_as_select_or_boolean_and_marks_the_current_model() {
    let session: Value = serde_json::from_str::<Value>(include_str!(
        "../tests/fixtures/claude-0.76.0/new-session.sanitized.json"
    ))
    .unwrap()["result"]
        .clone();
    let fast = fast_option(&session).unwrap();
    assert!(!fast.on);
    assert_eq!(fast.value(true), json!("on"));
    let models = config_models(&session, "claude");
    assert_eq!(
        models[0].fast,
        Some(ModelFast {
            on_by_default: false
        }),
        "the session's model, `default`, offers it"
    );
    assert!(models[1..].iter().all(|model| model.fast.is_none()));

    let boolean = json!({"configOptions":[{"id":"fast","category":"model_config",
        "type":"boolean","currentValue":true}]});
    let fast = fast_option(&boolean).unwrap();
    assert!(fast.on);
    assert_eq!(fast.value(false), json!(false));
    assert!(fast_option(&json!({"configOptions":[]})).is_none());
    let odd = json!({"configOptions":[{"id":"fast","type":"select","currentValue":"x",
        "options":[{"value":"x"}]}]});
    assert!(fast_option(&odd).is_none(), "a select without on and off");
}
