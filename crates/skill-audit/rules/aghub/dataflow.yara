// aghub data-flow correlation rules (behavioral-lite).
//
// These deliberately carry LOW/INFO severity on their own — a skill reading a
// secret, or making a network call, is not by itself malicious. They only
// matter when a `source` (reads secrets) and a `sink` (network egress) co-occur
// in the same skill, even across different files. engine::run() correlates that
// pair into a synthetic `aghub_dataflow_chain` finding (High → Suspicious).
//
// This is the cross-file case that single-file rules miss, e.g. cisco's
// multi-file-exfiltration eval (collector.py reads ~/.aws/credentials, a
// separate reporter.py POSTs it out). See OWASP ASI03/ASI04.

rule aghub_reads_secret {
	meta:
		author = "aghub"
		severity = "low"
		category = "credential_exfil"
		flow = "source"
		description = "reads credential files or harvests secret environment variables"
	strings:
		$cred_kw = /aws_secret_access_key|private[_-]?key|mnemonic|seed[_-]?phrase/ nocase
	condition:
		aghub_credential_source or $cred_kw
}

rule aghub_network_egress {
	meta:
		author = "aghub"
		severity = "info"
		category = "data_exfil"
		flow = "sink"
		description = "sends data to a network endpoint"
	strings:
		$http = /requests\.(post|put|patch)\s*\(|axios\b|\bfetch\s*\(|urllib|http\.client|httpx\.|\.post\s*\(/ nocase
		$socket = /socket\.socket|\.sendall\s*\(|\.send\s*\(/ nocase
		$cli = /\bcurl\b|\bwget\b/ nocase
	condition:
		any of them
}
