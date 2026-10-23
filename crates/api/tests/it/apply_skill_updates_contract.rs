use aghub_api::dto::skill::{
	ApplySkillUpdatesRequest, ApplySkillUpdatesResponse,
};

#[test]
fn bulk_apply_contract_is_camel_case_and_preserves_result_order() {
	let request: ApplySkillUpdatesRequest =
		serde_json::from_value(serde_json::json!({
			"source": "https://git.example/owner/repo.git",
			"names": ["alpha", "beta"],
			"scope": "project",
			"projectRoot": "/tmp/project",
			"confirm": true,
		}))
		.expect("batch request should deserialize");
	assert_eq!(request.source, "https://git.example/owner/repo.git");
	assert_eq!(request.names, ["alpha", "beta"]);
	assert_eq!(request.project_root.as_deref(), Some("/tmp/project"));

	let response = ApplySkillUpdatesResponse {
		results: vec![
			aghub_api::dto::skill::ApplySkillUpdateResponse {
				success: true,
				name: "alpha".to_string(),
				scope: "project".to_string(),
				updated_hash: Some("hash-a".to_string()),
				paths: Vec::new(),
				error: None,
				code: None,
			},
			aghub_api::dto::skill::ApplySkillUpdateResponse {
				success: false,
				name: "beta".to_string(),
				scope: "project".to_string(),
				updated_hash: None,
				paths: Vec::new(),
				error: Some("failed".to_string()),
				code: Some("TEST_FAILURE".to_string()),
			},
		],
	};
	let json =
		serde_json::to_value(response).expect("response should serialize");
	assert_eq!(json["results"][0]["name"], "alpha");
	assert_eq!(json["results"][1]["name"], "beta");
	assert_eq!(json["results"][0]["updatedHash"], "hash-a");
	assert!(json["results"][0].get("updated_hash").is_none());
}
