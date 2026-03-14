use mad_hatter::tunnel_service;
use mad_hatter::tunnel::{TunnelEndpoint, channel_pair, Dispatch};
use serde::{Serialize, Deserialize};
use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;

// ─── Test Service Definition ───

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct ContactInfo {
    name: String,
    id: String,
}

#[tunnel_service]
trait TestService: Send + Sync {
    async fn greet(&self, name: String) -> Result<String, String>;
    async fn add(&self, a: i32, b: i32) -> Result<i32, String>;
    async fn get_contacts(&self) -> Result<Vec<ContactInfo>, String>;
    async fn fail_always(&self) -> Result<(), String>;
}

// ─── Local Implementation ───

struct MyHandler;

#[async_trait]
impl TestService for MyHandler {
    async fn greet(&self, name: String) -> Result<String, String> {
        Ok(format!("Hello, {}!", name))
    }

    async fn add(&self, a: i32, b: i32) -> Result<i32, String> {
        Ok(a + b)
    }

    async fn get_contacts(&self) -> Result<Vec<ContactInfo>, String> {
        Ok(vec![
            ContactInfo { name: "Alice".into(), id: "a1".into() },
            ContactInfo { name: "Bob".into(), id: "b2".into() },
        ])
    }

    async fn fail_always(&self) -> Result<(), String> {
        Err("intentional error".to_string())
    }
}

// ─── Helper: create connected endpoints ───

fn create_endpoints(
    dispatchers_a: Vec<Box<dyn Dispatch>>,
    dispatchers_b: Vec<Box<dyn Dispatch>>,
) -> (Arc<TunnelEndpoint>, Arc<TunnelEndpoint>) {
    let ((incoming_a, outgoing_a), (incoming_b, outgoing_b)) = channel_pair();
    let timeout = Duration::from_secs(5);

    let endpoint_a = TunnelEndpoint::new(
        incoming_a,
        outgoing_a,
        dispatchers_a,
        timeout,
    );

    let endpoint_b = TunnelEndpoint::new(
        incoming_b,
        outgoing_b,
        dispatchers_b,
        timeout,
    );

    (endpoint_a, endpoint_b)
}

// ─── Tests ───

#[tokio::test]
async fn test_basic_rpc_single_param() {
    let handler = Arc::new(MyHandler);
    let (endpoint_a, _endpoint_b) = create_endpoints(
        vec![],
        vec![TestServiceDispatcher::boxed(handler)],
    );

    let proxy = TestServiceProxy::new(endpoint_a);
    let result = proxy.greet("World".to_string()).await;
    assert_eq!(result.unwrap(), "Hello, World!");
}

#[tokio::test]
async fn test_basic_rpc_multi_param() {
    let handler = Arc::new(MyHandler);
    let (endpoint_a, _endpoint_b) = create_endpoints(
        vec![],
        vec![TestServiceDispatcher::boxed(handler)],
    );

    let proxy = TestServiceProxy::new(endpoint_a);
    let result = proxy.add(3, 4).await;
    assert_eq!(result.unwrap(), 7);
}

#[tokio::test]
async fn test_rpc_no_param_complex_return() {
    let handler = Arc::new(MyHandler);
    let (endpoint_a, _endpoint_b) = create_endpoints(
        vec![],
        vec![TestServiceDispatcher::boxed(handler)],
    );

    let proxy = TestServiceProxy::new(endpoint_a);
    let contacts = proxy.get_contacts().await.unwrap();
    assert_eq!(contacts.len(), 2);
    assert_eq!(contacts[0].name, "Alice");
    assert_eq!(contacts[1].id, "b2");
}

#[tokio::test]
async fn test_rpc_error_propagation() {
    let handler = Arc::new(MyHandler);
    let (endpoint_a, _endpoint_b) = create_endpoints(
        vec![],
        vec![TestServiceDispatcher::boxed(handler)],
    );

    let proxy = TestServiceProxy::new(endpoint_a);
    let result = proxy.fail_always().await;
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "intentional error");
}

#[tokio::test]
async fn test_bidirectional_rpc() {
    let handler_a = Arc::new(MyHandler);
    let handler_b = Arc::new(MyHandler);

    let (endpoint_a, endpoint_b) = create_endpoints(
        vec![TestServiceDispatcher::boxed(handler_a)],
        vec![TestServiceDispatcher::boxed(handler_b)],
    );

    // A calls B
    let proxy_a = TestServiceProxy::new(endpoint_a.clone());
    assert_eq!(proxy_a.greet("from A".to_string()).await.unwrap(), "Hello, from A!");

    // B calls A
    let proxy_b = TestServiceProxy::new(endpoint_b.clone());
    assert_eq!(proxy_b.greet("from B".to_string()).await.unwrap(), "Hello, from B!");
}

#[tokio::test]
async fn test_multiple_sequential_calls() {
    let handler = Arc::new(MyHandler);
    let (endpoint_a, _endpoint_b) = create_endpoints(
        vec![],
        vec![TestServiceDispatcher::boxed(handler)],
    );

    let proxy = TestServiceProxy::new(endpoint_a);
    for i in 0..10 {
        let result = proxy.add(i, i * 2).await;
        assert_eq!(result.unwrap(), i + i * 2);
    }
}

#[tokio::test]
async fn test_concurrent_calls() {
    let handler = Arc::new(MyHandler);
    let (endpoint_a, _endpoint_b) = create_endpoints(
        vec![],
        vec![TestServiceDispatcher::boxed(handler)],
    );

    let mut handles = vec![];
    for i in 0..5i32 {
        let ep = endpoint_a.clone();
        handles.push(tokio::spawn(async move {
            let proxy = TestServiceProxy::new(ep);
            proxy.add(i, i + 1).await.unwrap()
        }));
    }

    let mut results = vec![];
    for h in handles {
        results.push(h.await.unwrap());
    }
    for (i, result) in results.into_iter().enumerate() {
        let i = i as i32;
        assert_eq!(result, i + i + 1);
    }
}

// ─── Second Service (multi-trait test) ───

#[tunnel_service]
trait MathService: Send + Sync {
    async fn multiply(&self, a: i32, b: i32) -> Result<i32, String>;
}

struct MathHandler;

#[async_trait]
impl MathService for MathHandler {
    async fn multiply(&self, a: i32, b: i32) -> Result<i32, String> {
        Ok(a * b)
    }
}

#[tokio::test]
async fn test_multi_trait_dispatch() {
    let test_handler = Arc::new(MyHandler);
    let math_handler = Arc::new(MathHandler);

    let (endpoint_a, _endpoint_b) = create_endpoints(
        vec![],
        vec![
            TestServiceDispatcher::boxed(test_handler),
            MathServiceDispatcher::boxed(math_handler),
        ],
    );

    let test_proxy = TestServiceProxy::new(endpoint_a.clone());
    let math_proxy = MathServiceProxy::new(endpoint_a.clone());

    assert_eq!(test_proxy.greet("multi".to_string()).await.unwrap(), "Hello, multi!");
    assert_eq!(math_proxy.multiply(6, 7).await.unwrap(), 42);
}

