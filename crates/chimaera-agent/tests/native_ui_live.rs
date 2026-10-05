//! No model request: exercises the real Claude Mod engine and driver transport.
use chimaera_agent::{
    claude::{chat_args, ClaudeAdapter},
    driver::SpawnSpec,
    native_ui::{NativeUiCommand, NativeUiEvent},
    ChatManager,
};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::broadcast;

struct Running(Arc<ChatManager>);
impl Drop for Running {
    fn drop(&mut self) {
        self.0.kill("mods");
    }
}

async fn rpc(
    manager: &ChatManager,
    receiver: &mut broadcast::Receiver<Arc<NativeUiEvent>>,
    n: &mut u32,
    request: Value,
) -> Value {
    *n += 1;
    let id = n.to_string();
    manager
        .native_ui(
            "mods",
            NativeUiCommand::new("fixture", &id, request).unwrap(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(40), async {
        loop {
            let frame = receiver.recv().await.unwrap();
            if let NativeUiEvent::Response {
                request_id,
                result,
                error,
                ..
            } = &*frame
            {
                if request_id == &id {
                    assert!(error.is_none(), "native UI error: {error:?}");
                    return result.clone().unwrap();
                }
            }
        }
    })
    .await
    .expect("native Mod response")
}

fn node<'a>(tree: &'a Value, kind: &str) -> &'a Value {
    tree["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == kind)
        .unwrap()
}

#[tokio::test]
#[ignore = "live: installed Claude 2.1.288 Mod engine; no model calls or billing"]
async fn claude_mod_controls_modules_and_reattach() {
    let directory = tempfile::tempdir().unwrap();
    let manager = Arc::new(ChatManager::new(
        directory.path().join("journal"),
        Box::new(|_, _| {}),
        Box::new(|_, _| {}),
    ));
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-mod");
    let mut args = vec!["claude".into()];
    args.extend(chat_args(Some("haiku"), None));
    args.extend([
        "--plugin-dir".into(),
        fixture.to_string_lossy().into_owned(),
        "--strict-mcp-config".into(),
        "--mcp-config".into(),
        "{\"mcpServers\":{}}".into(),
    ]);
    manager
        .spawn(
            &ClaudeAdapter,
            SpawnSpec::new("mods", args, directory.path().into()),
        )
        .unwrap();
    let _running = Running(manager.clone());
    let mut receiver = manager.attach("mods", 0).unwrap().native_ui;
    let mut n = 0;
    rpc(
        &manager,
        &mut receiver,
        &mut n,
        json!({"subtype":"ui_attach"}),
    )
    .await;
    let mut panes = Value::Null;
    for _ in 0..20 {
        panes = rpc(
            &manager,
            &mut receiver,
            &mut n,
            json!({"subtype":"ui_panes"}),
        )
        .await;
        if panes["panes"].as_array().is_some_and(|p| !p.is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(panes["panes"][0]["id"], "chimaera-test");
    let render =
        json!({"subtype":"ui_render","component":"Pane","instance_id":"chimaera-test","props":{}});
    let first = rpc(&manager, &mut receiver, &mut n, render.clone()).await;
    assert_eq!(first["hooked"], true);
    let button = node(&first["tree"], "Button");
    let pressed = rpc(
        &manager,
        &mut receiver,
        &mut n,
        json!({"subtype":"ui_press","plugin":button["press"]["plugin"],
        "handle":button["press"]["handle"],"key":button["props"]["key"]}),
    )
    .await;
    assert_eq!(pressed["handled"], true);
    // Engine invalidations coalesce for 100ms; a render before the push may
    // legitimately return the preceding cached tree.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = rpc(&manager, &mut receiver, &mut n, render.clone()).await;
    assert_eq!(
        second["tree"]["children"][1]["children"][0],
        "Count: 1 / Initial / one"
    );
    let field = node(&second["tree"], "Input");
    rpc(&manager, &mut receiver, &mut n, json!({"subtype":"ui_input","plugin":field["press"]["plugin"],
        "handle":field["press"]["handle"],"key":"name","component":"Pane","instance_id":"chimaera-test","kind":"change","value":"Changed"})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let third = rpc(&manager, &mut receiver, &mut n, render.clone()).await;
    let select = node(&third["tree"], "Select");
    rpc(&manager, &mut receiver, &mut n, json!({"subtype":"ui_select","plugin":select["press"]["plugin"],
        "handle":select["press"]["handle"],"key":"choice","component":"Pane","instance_id":"chimaera-test","value":"two"})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let module = rpc(
        &manager,
        &mut receiver,
        &mut n,
        json!({"subtype":"ui_client_module","plugin":"chimaera-ui-probe"}),
    )
    .await;
    assert_eq!(module["runtime"], "claude:surface-runtime");
    assert_eq!(module["modules"][0]["module"], "hooks/counter.tsx");
    assert!(module["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|file| file["key"] == "claude:surface-runtime"));
    rpc(
        &manager,
        &mut receiver,
        &mut n,
        json!({"subtype":"ui_detach"}),
    )
    .await;
    rpc(
        &manager,
        &mut receiver,
        &mut n,
        json!({"subtype":"ui_attach"}),
    )
    .await;
    let restored = rpc(&manager, &mut receiver, &mut n, render).await;
    assert_eq!(
        restored["tree"]["children"][1]["children"][0],
        "Count: 1 / Changed / two"
    );
    let closed = rpc(
        &manager,
        &mut receiver,
        &mut n,
        json!({"subtype":"ui_close","id":"chimaera-test"}),
    )
    .await;
    assert_eq!(closed["closed"], true);
}
