use skills_sh::{Client, ClientBuilder, SearchParams};
use std::time::Duration;

#[test]
fn test_client_creation() {
	let client = Client::new();
	assert!(client.is_ok());
}

#[test]
fn test_client_builder_chain() {
	let client = ClientBuilder::new()
		.api_url("https://api.example.com/v1")
		.timeout(Duration::from_secs(5))
		.build();

	assert!(client.is_ok());
}

#[tokio::test]
async fn test_search_params_builder() {
	let params = SearchParams::new("git").with_limit(10);
	assert_eq!(params.query, "git");
	assert_eq!(params.limit, Some(10));
}

#[tokio::test]
#[ignore = "Flaky remote API test"]
async fn test_live_search() {
	let client = Client::new().unwrap();
	let results = client.find("github").await.unwrap();
	assert!(!results.is_empty());
	let first = &results[0];
	assert!(!first.name.is_empty());
	assert!(!first.slug.is_empty());
	assert!(first.installs > 0);
}
