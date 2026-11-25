//! Its own test binary: it sets proxy variables for the whole process.

use dictly::{config::Config, pipeline::Pipeline};
use std::time::Duration;
use tiny_http::{Response, Server};

#[test]
fn loopback_http_is_never_sent_through_an_environment_proxy() {
    let proxy = Server::http("127.0.0.1:0").unwrap();
    let provider = Server::http("127.0.0.1:0").unwrap();
    let proxy_url = format!("http://{}", proxy.server_addr());
    // SAFETY: set before any other thread of this test binary reads the environment.
    unsafe {
        std::env::set_var("HTTP_PROXY", &proxy_url);
        std::env::set_var("ALL_PROXY", &proxy_url);
        std::env::remove_var("NO_PROXY");
    }
    let config = Config {
        base_url: format!("http://{}/v1", provider.server_addr()),
        api_key_env: String::new(),
        ..Config::default()
    };
    let worker = std::thread::spawn(move || {
        let request = provider
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .expect("the provider must receive the request directly");
        request
            .respond(Response::from_string(r#"{"text":"Direct."}"#))
            .unwrap();
    });
    let result = Pipeline::with_api_key(config, Some("sk-canary".into()))
        .unwrap()
        .transcribe(vec![1; 64], "voice.wav");
    assert!(
        proxy
            .recv_timeout(Duration::from_millis(100))
            .unwrap()
            .is_none(),
        "The audio and API key were sent to the proxy"
    );
    worker.join().unwrap();
    assert_eq!(result.unwrap().text, "Direct.");
}
