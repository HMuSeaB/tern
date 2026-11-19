use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{json, Value};
use tern_gateway::{
    ApiFormat, Gateway, GatewayConfig, PluginRef, ProviderAuth, ProviderSpec, UpstreamProxy,
};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
}

fn start_python(script: &str, args: &[String]) -> ChildGuard {
    let child = Command::new(python())
        .arg("-u")
        .arg("-c")
        .arg(script)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("Python 3 is required for plugin_e2e");
    ChildGuard(child)
}

fn available_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn provider(plugin_dir: &std::path::Path) -> ProviderSpec {
    let mut provider = ProviderSpec::new(
        "demo",
        "Demo plugin",
        "http://127.0.0.1:0/v1",
        ApiFormat::OpenaiChat,
        ProviderAuth::None,
    );
    provider.plugin = Some(PluginRef {
        plugin_id: "demo".into(),
        dir: Some(plugin_dir.to_string_lossy().into_owned()),
    });
    provider
}

#[tokio::test]
async fn plugin_sidecar_is_started_lazily_and_receives_only_plugin_auth_upstream() {
    let upstream_port = available_port();
    let upstream_script = r#"
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
class H(BaseHTTPRequestHandler):
 def log_message(self, *args): pass
 def do_POST(self):
  request=json.loads(self.rfile.read(int(self.headers.get('Content-Length','0'))))
  result={'authorization':self.headers.get('Authorization'),'plugin_token':self.headers.get('X-Tern-Plugin-Token'),'cookie':self.headers.get('Cookie'),'model':request.get('model'),'path':self.path}
  body=json.dumps({'id':'chatcmpl-plugin-e2e','object':'chat.completion','created':1,'model':request.get('model'),'choices':[{'index':0,'message':{'role':'assistant','content':json.dumps(result)},'finish_reason':'stop'}],'usage':{'prompt_tokens':3,'completion_tokens':4,'total_tokens':7}}).encode()
  self.send_response(200); self.send_header('Content-Type','application/json'); self.send_header('Content-Length',str(len(body))); self.end_headers(); self.wfile.write(body)
HTTPServer(('127.0.0.1',int(sys.argv[1])),H).serve_forever()
"#;
    let _upstream = start_python(upstream_script, &[upstream_port.to_string()]);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let root = tempfile::tempdir().unwrap();
    let plugin_dir = root.path().join("demo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/tern-plugin-workbuddy/tern-plugin-workbuddy.py");
    std::fs::copy(source, plugin_dir.join("tern-plugin-workbuddy.py")).unwrap();
    std::fs::write(
        plugin_dir.join("session.json"),
        r#"{"token":"plugin-upstream-session-secret"}"#,
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(plugin_dir.join("session.json"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(plugin_dir.join("session.json"), permissions).unwrap();
    }
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        format!(
            "id = \"demo\"\nname = \"Demo\"\ncommand = \"{}\"\nargs = [\"tern-plugin-workbuddy.py\", \"serve\", \"--upstream\", \"http://127.0.0.1:{upstream_port}/v1\"]\napi_format = \"openai_chat\"\nmodels = [\"test-model\"]\n",
            python()
        ),
    )
    .unwrap();

    let mut config = GatewayConfig::new(vec![provider(&plugin_dir)]);
    config.upstream_proxy = UpstreamProxy::Direct;
    let plugins = std::sync::Arc::new(tern_gateway::PluginSupervisor::new(root.path()).unwrap());
    let gateway = Gateway::new(config)
        .unwrap()
        .with_plugin_supervisor(plugins.clone());
    let gateway_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let gateway_addr = gateway_listener.local_addr().unwrap();
    let gateway_task = tokio::spawn(
        gateway
            .clone()
            .serve_on(gateway_listener, std::future::pending::<()>()),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Manifest-declared plugin models are listed without starting the sidecar.
    let listed = reqwest::Client::new()
        .get(format!("http://{gateway_addr}/v1/models"))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed: Value = listed.json().await.unwrap();
    assert_eq!(listed["data"][0]["id"], "demo/test-model");

    // Gateway::new does not eagerly launch the sidecar; routing the first request does.
    let response = reqwest::Client::new()
        .post(format!("http://{gateway_addr}/v1/messages"))
        .header("Authorization", "Bearer client-secret-must-not-leak")
        .header("Cookie", "client-cookie-must-not-leak")
        .header("X-Tern-Plugin-Token", "attacker-supplied-token")
        .json(&json!({
            "model": "demo/test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hello"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let response: Value = response.json().await.unwrap();
    let details: Value = serde_json::from_str(
        response["content"][0]["text"]
            .as_str()
            .expect("assistant text"),
    )
    .unwrap();
    assert_eq!(
        details["authorization"],
        "Bearer plugin-upstream-session-secret"
    );
    assert!(details["plugin_token"].is_null());
    assert!(details["cookie"].is_null());
    assert_eq!(details["model"], "test-model");
    assert_eq!(details["path"], "/v1/chat/completions");
    assert_eq!(response["usage"]["input_tokens"], 3);
    assert_eq!(response["usage"]["output_tokens"], 4);

    // A crashed/stopped child does not take down the gateway; the next request relaunches it.
    plugins.stop("demo").await;
    let restarted = reqwest::Client::new()
        .post(format!("http://{gateway_addr}/v1/messages"))
        .header("Authorization", "Bearer client-secret-must-not-leak")
        .json(&json!({
            "model": "demo/test-model",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "again"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(restarted.status(), reqwest::StatusCode::OK);

    gateway_task.abort();
    let _ = gateway_task.await;
}
