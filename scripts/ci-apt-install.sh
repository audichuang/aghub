#!/usr/bin/env bash
# Install apt packages on a GitHub-hosted Ubuntu runner with a bounded download.
# The runner's azure mirror sometimes serves at ~60 kB/s, turning a 10s download
# into 20 minutes; a timed-out attempt retries against archive.ubuntu.com.
# docs/history/tooling.md#ci-apt-tail-and-cargo-index
set -euo pipefail

sudo apt-get update
for attempt in 1 2 3; do
	# Download only, so a timeout never interrupts dpkg mid-install.
	if sudo timeout 90 apt-get install -y --download-only "$@"; then
		sudo apt-get install -y "$@"
		exit 0
	fi
	[ "$attempt" -eq 3 ] && break
	echo "::warning::apt download attempt $attempt failed or timed out; switching off the azure mirror"
	sudo sed -i 's#azure\.archive\.ubuntu\.com#archive.ubuntu.com#g' \
		/etc/apt/apt-mirrors.txt /etc/apt/sources.list /etc/apt/sources.list.d/* 2>/dev/null || true
	# A failed refresh must not end the loop: the next download is the real check.
	sudo apt-get update || true
done
exit 1
