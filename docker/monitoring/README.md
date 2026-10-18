# Observability v2 (U-PE-10)

This overlay extends **root** `docker-compose.yml`; it is not a standalone deployment.
It replaces the old independent database/application/Jaeger stack. The existing Tempo
service is overridden, not duplicated. No production database or role changes occur.
Image tags and multi-architecture index digests are recorded in `images.json`, checked
against official publisher metadata. Collector/Prometheus/Grafana/Loki/Pyroscope/
Alertmanager follow the checked stable releases. Tempo remains on the compatible
2.7.2 single-binary/local-block line; a Tempo 3 storage migration is separate and
this pin is not a claim that Tempo is current or vulnerability-free.
All monitoring services are opt-in (`observability`), with 64–256 MiB per-service
limits, 24-hour local retention and bounded collector queues. Budget approximately
1.35 GiB extra; check actual usage before enabling on a small machine.

From repository root, following the existing ownership/preflight procedure:

```sh
# Public variable names only; values are operator-owned external files.
# MONITORING_METRICS_TOKEN_FILE: readable bearer credential authorized for
# GatewayMetricsRead; no admin/DSN fallback. Respect its expiry and rotation.
# MONITORING_GRAFANA_PASSWORD_FILE: readable Grafana admin password file.
docker compose -f docker-compose.yml -f docker/monitoring/docker-compose.monitoring.yml --profile observability config --quiet
docker compose -f docker-compose.yml -f docker/monitoring/docker-compose.monitoring.yml --profile observability up -d
```

Compose >=2.24.4 is required for `!reset`: inherited Tempo host ports are removed.
Only Grafana is published, on loopback port 3001. No ingestion, Prometheus, Loki or
Tempo API is exposed to the browser. Optional profiling adds
`-f docker/monitoring/docker-compose.profiling.yml --profile profiling`; the agent
must explicitly send profiles to `http://pyroscope:4040`. No automatic profiling
instrumentation is claimed.

Native Gateway/Knowledge and the explicitly enabled Runtime/Worker exporters all use
HTTP/protobuf at `otel-collector:4318`. The collector
exports traces only to Tempo, logs through Loki's `/otlp/v1/logs`, and metrics via
its Prometheus exporter `:8889`. Prometheus also scrapes the **authenticated**
Gateway `:8080/metrics`, using an external credential file. Redis `:6379` is not
an HTTP metrics exporter. Additional service metrics must arrive through their
existing OTLP implementation; nonexistent `/metrics` endpoints are not invented.

The collector removes arbitrary resource/span/log/metric attributes using a strict
allowlist, removes span events, replaces span names/status descriptions and log
bodies, and never exports to debug/stdout. Prompt/response capture is off. This is
an **operator-only** local monitoring store: Loki single-process auth is disabled
inside the app network, Grafana anonymous/signup is disabled, and telemetry is not
a tenant-query API. Do not expose these stores to tenants or the public network.
The authenticated legacy Prometheus scrape retains source label dimensions, including
`path`: deleting labels without aggregation would create duplicate series and lose
scrapes. Treat this privileged metrics store as potentially identifying data.
Tenant self-service requires separately authenticated routing/storage isolation;
never trust client-supplied tenant headers. Span-link attributes and allowlisted
string values still require trusted instrumentation; this profile is not an
untrusted arbitrary-OTLP ingestion boundary.

Provisioned dashboard: API request rate, availability/latency burn, admission
rejections, scrape health, P95 latency. API SLO is 99.9% availability; request latency
SLO is 99% below **2500 ms**, the actual exported histogram boundary. Both use paired
5m/1h (14.4x) and 30m/6h (6x) burn alerts. No traffic does not burn error budget;
missing telemetry alerts separately. Streaming TTFT, agent-turn, retrieval and
ingestion SLOs require U-PE-11 measured counters/histograms and remain unaccepted;
these request latency alerts do not stand in for them. Alertmanager intentionally
has no external receiver: configure an operator-owned receiver before claiming
paging acceptance. The old nonexistent Gateway webhook is removed.

Offline source contracts (native `platform-tools`; no containers/network):

```sh
platform-tools observability-check --repo-root .
# When the pinned Prometheus distribution's promtool is available:
cd docker/monitoring
promtool check config prometheus.yml
promtool check rules alert-rules.yml
promtool test rules alert-rules.test.yml
```

`check config` needs the deployment's mounted `/etc/prometheus` files and credential
path; run it in the provisioned tool environment. Offline rule syntax/unit tests
operate directly in this directory. Validate collector config with its pinned
binary before activation. Static tests cover duplicate backends, accidental default
enablement/public ports, missing auth/redaction/signals, and weakened SLO windows.
They do not validate Collector OTTL or Compose merge semantics. The earlier Python
8/8 execution is retained only as historical evidence outside the repository; the
Python operational tool was removed. Rust contract tests require the primary
writer’s registered module/build before execution.

Remaining real acceptance: rendered merge and pinned-binary config checks; actual
memory/retention; service OTLP export and W3C parentage through real journeys;
redaction fixtures observed at storage; credential expiry/rotation; burn/resolve
alerts delivered to an operator receiver. No deployment or live trace acceptance
is implied by static validation.

Every observability/profiling container now declares a numeric non-root identity, read-only
root, dropped capabilities, no-new-privileges, PID bounds and bounded noexec scratch. Writable
state remains on the existing named data volumes. Before an authorized start, inspect and
prepare those volumes for their declared UID/GID; no root repair container or volume deletion
is introduced. Prometheus/Alertmanager use 65534, Grafana 472, Collector/Loki/Tempo/Pyroscope
10001. External credential files must be readable by the specific consumer. Grafana logs use
its existing writable data volume. These source settings still require exact-image filesystem
and memory acceptance. `platform-tools platform-fitness` adds negative source checks for this
previously uncovered profile, without substituting them for a rendered Compose/network test.

Candidate GenAI/retrieval/ingestion latency recording rules and paired burn alerts now use the
actual exported histogram names and operation labels from U-PE-11 instrumentation. Provisional
operational targets are 99% of observed first-text-chunks/retrieval operations within 5 seconds,
and agent-turn/ingestion operations within 50 seconds (native SDK bucket boundaries). These are
operator review inputs, not replacements for the enterprise performance contract. First text
chunk is not browser first byte. Missing counters or required buckets alert separately; idle
traffic never burns the latency budget. The earlier Gateway idle-traffic slow-ratio issue is
also corrected and has a zero-traffic negative rule fixture. Additional dashboard panels show
provider observation latency and actual settled token/cost counters by opaque tenant/model.
These queries, buckets, provisional targets and burn/resolve fixtures still need `promtool`,
actual Collector output and real operator acceptance; no newly added rule is a measured SLO.
