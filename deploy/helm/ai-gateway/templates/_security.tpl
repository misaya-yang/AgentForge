{{/* Exact native TLS peer graph; never a forwarded identity header. */}}
{{- define "ai-gateway.workloadID" -}}
spiffe://{{ .root.Values.workloadIdentity.trustDomain }}/service/{{ .service }}
{{- end -}}
{{- define "ai-gateway.internalOrigin" -}}
{{- $ports := dict "gateway" 8080 "agent-runtime" 8094 "agent-capability-worker" 8095 "knowledge" 8092 "knowledge-worker" 8092 "sdk" 8096 -}}
{{- $tls := dict "gateway" 8443 "agent-runtime" 8454 "agent-capability-worker" 8455 "knowledge" 8452 "knowledge-worker" 8452 "sdk" 8456 -}}
{{- $secure := ne .root.Values.workloadIdentity.mode "disabled" -}}
{{ ternary "https" "http" $secure }}://{{ include "ai-gateway.fullname" .root }}-{{ .service }}:{{ ternary (index $tls .service) (index $ports .service) $secure }}
{{- end -}}
{{- define "ai-gateway.workloadEnv" -}}
{{- if ne .root.Values.workloadIdentity.mode "disabled" }}
{{- $services := dict "gateway" (list "agent-runtime" "agent-capability-worker" "knowledge" "sdk") "agent-runtime" (list "gateway" "agent-capability-worker" "knowledge") "agent-capability-worker" (list "gateway" "knowledge") "knowledge" (list "gateway") "knowledge-worker" (list "gateway") "sdk" (list "gateway") }}
{{- $clients := dict "gateway" (list "agent-runtime" "agent-capability-worker" "knowledge" "knowledge-worker" "sdk") "agent-runtime" (list "gateway") "agent-capability-worker" (list "gateway" "agent-runtime") "knowledge" (list "gateway" "agent-capability-worker") "knowledge-worker" (list "gateway") "sdk" (list "gateway") }}
{{- $ports := dict "gateway" 8443 "agent-runtime" 8454 "agent-capability-worker" 8455 "knowledge" 8452 "knowledge-worker" 8452 "sdk" 8456 }}
{{- $origins := dict }}{{- $ids := list }}
{{- range $server := index $services .service }}
{{- if or (ne $server "sdk") $.root.Values.sdkWorker.enabled }}
{{- $_ := set $origins (include "ai-gateway.internalOrigin" (dict "root" $.root "service" $server)) (include "ai-gateway.workloadID" (dict "root" $.root "service" $server)) }}
{{- end }}{{- end }}
{{- range $client := index $clients .service }}
{{- if or (ne $client "sdk") $.root.Values.sdkWorker.enabled }}
{{- $ids = append $ids (include "ai-gateway.workloadID" (dict "root" $.root "service" $client)) }}
{{- end }}{{- end }}
- name: POD_IP
  valueFrom: {fieldRef: {fieldPath: status.podIP}}
- {name: AI_PLATFORM_WORKLOAD_MTLS_MODE, value: {{ .root.Values.workloadIdentity.mode | quote }}}
- {name: AI_PLATFORM_WORKLOAD_TLS_BIND, value: "$(POD_IP):{{ index $ports .service }}"}
- {name: AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR, value: /run/workload/credentials}
- name: AI_PLATFORM_WORKLOAD_SPIFFE_ID
  value: {{ include "ai-gateway.workloadID" . | quote }}
- name: AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS
  value: {{ join "," $ids | quote }}
{{- if eq .service "gateway" }}
- name: AI_PLATFORM_WORKLOAD_KNOWLEDGE_CLIENT_IDS
  value: {{ printf "%s,%s" (include "ai-gateway.workloadID" (dict "root" .root "service" "knowledge")) (include "ai-gateway.workloadID" (dict "root" .root "service" "knowledge-worker")) | quote }}
{{- end }}
- name: AI_PLATFORM_WORKLOAD_ALLOWED_ORIGINS
  value: {{ $origins | toJson | quote }}
{{- if not (has .service (list "knowledge" "knowledge-worker")) }}
- name: AI_PLATFORM_WORKLOAD_PEERS_JSON
  value: {{ $origins | toJson | quote }}
{{- end }}
{{- if or (eq .service "knowledge") (eq .service "knowledge-worker") }}
- name: KNOWLEDGE_GATEWAY_HEALTH_ORIGIN
  value: http://{{ include "ai-gateway.fullname" .root }}-gateway:8080
{{- end }}
{{- if eq .service "agent-runtime" }}
- {name: AI_PLATFORM_RUNTIME_MODEL_PROXY_BASE_URL, value: "http://127.0.0.1:8094/internal/v1/model-proxy"}
{{- end }}
{{- end }}
{{- end -}}
{{- define "ai-gateway.workloadImporter" -}}
{{- if ne .root.Values.workloadIdentity.mode "disabled" }}
- name: {{ ternary "workload-import" "workload-rotation" .once }}
  image: {{ include "ai-gateway.image" (dict "root" .root "unit" "tools") | quote }}
  command: [/bin/sh, /opt/workload/import.sh]
  args: [{{ ternary "once" "watch" .once | quote }}]
  securityContext:
{{ include "ai-gateway.security" .uid | indent 4 }}
  resources: {requests: {cpu: 10m, memory: 32Mi}, limits: {cpu: 100m, memory: 64Mi}}
  env:
  - name: WORKLOAD_SPIFFE_ID
    value: {{ include "ai-gateway.workloadID" . | quote }}
  - name: WORKLOAD_ROTATION_SECONDS
    value: {{ .root.Values.workloadIdentity.rotationPollSeconds | quote }}
  volumeMounts:
  - {name: workload-input, mountPath: /input, readOnly: true}
  - {name: workload-private, mountPath: /run/workload}
  - {name: workload-importer, mountPath: /opt/workload, readOnly: true}
  - {name: tmp, mountPath: /tmp}
{{- if not .once }}
  readinessProbe:
    exec: {command: [/bin/sh, /opt/workload/import.sh, ready]}
    periodSeconds: {{ .root.Values.workloadIdentity.rotationPollSeconds }}
{{- end }}
{{- end }}
{{- end -}}
{{- define "ai-gateway.workloadMount" -}}
{{- if ne .Values.workloadIdentity.mode "disabled" }}
- {name: workload-private, mountPath: /run/workload, readOnly: true}
{{- end }}
{{- end -}}
{{- define "ai-gateway.workloadVolumes" -}}
{{- if ne .root.Values.workloadIdentity.mode "disabled" }}
- name: workload-input
  secret:
    secretName: {{ index .root.Values.workloadIdentity.secrets .role | quote }}
    defaultMode: 0440
    items: [{key: ca.pem, path: ca.pem}, {key: cert.pem, path: cert.pem}, {key: key.pem, path: key.pem}]
- name: workload-private
  emptyDir: {medium: Memory, sizeLimit: 64Mi}
- name: workload-importer
  configMap: {name: {{ include "ai-gateway.fullname" .root }}-workload-importer, defaultMode: 0444}
{{- end }}
{{- end -}}
{{- define "ai-gateway.dataEnv" -}}
{{- if .root.Values.dataSecurity.enabled }}
{{- $c := .root.Values.dataSecurity }}
{{- $alias := ternary $c.physicalDatabase (printf "%s_session" .alias) (eq $c.mode "direct") }}
{{- range $suffix, $file := dict "DSN_FILE" "dsn" "TLS_CA_FILE" "ca-bundle.pem" "TLS_CERT_FILE" "tls.crt" "TLS_KEY_FILE" "tls.key" }}
- {name: {{ $.prefix }}_{{ $suffix }}, value: /run/data/credentials/{{ $file }}}
{{- end }}
- {name: {{ .prefix }}_MODE, value: {{ $c.mode | quote }}}
- {name: {{ .prefix }}_EXPECTED_HOST, value: {{ $c.expectedHost | quote }}}
- {name: {{ .prefix }}_DATABASE_ALIAS, value: {{ $alias | quote }}}
- {name: {{ .prefix }}_PHYSICAL_DATABASE, value: {{ $c.physicalDatabase | quote }}}
- {name: {{ .prefix }}_POOL_MAX, value: {{ $c.poolMax | quote }}}
- {name: {{ .prefix }}_REPLICAS, value: {{ .replicas | quote }}}
- {name: {{ .prefix }}_ROLE_CONNECTION_LIMIT, value: {{ $c.roleConnectionLimit | quote }}}
- {name: {{ .prefix }}_POOLED_BACKEND_LIMIT, value: {{ $c.pooledBackendLimit | quote }}}
- {name: {{ .prefix }}_STATEMENT_TIMEOUT_MS, value: {{ $c.statementTimeoutMs | quote }}}
{{- if eq .role "gateway" }}
{{/* Cancellation always bypasses PgBouncer using the same role. */}}
{{- range $suffix, $file := dict "DSN_FILE" "control-dsn" "TLS_CA_FILE" "ca-bundle.pem" "TLS_CERT_FILE" "tls.crt" "TLS_KEY_FILE" "tls.key" }}
- {name: DATA_GATEWAY_CONTROL_{{ $suffix }}, value: /run/data/credentials/{{ $file }}}
{{- end }}
- {name: DATA_GATEWAY_CONTROL_MODE, value: direct}
- {name: DATA_GATEWAY_CONTROL_EXPECTED_HOST, value: {{ .root.Values.dataSecurity.controlHost | default "postgres" | quote }}}
- {name: DATA_GATEWAY_CONTROL_DATABASE_ALIAS, value: {{ $c.physicalDatabase | quote }}}
- {name: DATA_GATEWAY_CONTROL_PHYSICAL_DATABASE, value: {{ $c.physicalDatabase | quote }}}
- {name: DATA_GATEWAY_CONTROL_POOL_MAX, value: "2"}
- {name: DATA_GATEWAY_CONTROL_REPLICAS, value: "1"}
- {name: DATA_GATEWAY_CONTROL_ROLE_CONNECTION_LIMIT, value: {{ $c.roleConnectionLimit | quote }}}
{{- end }}
{{- end }}
{{- end -}}
{{- define "ai-gateway.dataMount" -}}
{{- if .Values.dataSecurity.enabled }}
{{- range $file := list "dsn" "ca-bundle.pem" "tls.crt" "tls.key" }}
- {name: data-credentials, mountPath: /run/data/credentials/{{ $file }}, subPath: {{ $file }}, readOnly: true}
{{- end }}
{{- if eq .Values.dataSecurity.mode "session" }}
- {name: data-credentials, mountPath: /run/data/credentials/authority-dsn, subPath: authority-dsn, readOnly: true}
{{- end }}
{{- end }}
{{- end -}}
{{- define "ai-gateway.dataVolumes" -}}
{{- if .root.Values.dataSecurity.enabled }}
- name: data-credentials
  secret:
    secretName: {{ index .root.Values.dataSecurity.roles .role | quote }}
    defaultMode: 0440
    items:
    - {key: dsn, path: dsn}
{{- if and (eq .root.Values.dataSecurity.mode "session") (ne .role "migrator") }}
    - {key: authority-dsn, path: authority-dsn}
{{- end }}
    - {key: ca-bundle.pem, path: ca-bundle.pem}
    - {key: tls.crt, path: tls.crt}
    - {key: tls.key, path: tls.key}
{{- if eq .role "gateway" }}
    - {key: control-dsn, path: control-dsn}
{{- end }}
{{- end }}
{{- end -}}
{{- define "ai-gateway.appSecretKeys" -}}
{{- $keys := list }}
{{- if eq .root.Values.workloadIdentity.mode "dual" }}{{- $keys = append $keys "AI_PLATFORM_INTERNAL_TOKEN" }}{{- end }}
{{- if eq .role "gateway" }}{{- $keys = concat $keys (list "GATEWAY_AUTHENTICATION__JWT__SECRET" "GATEWAY_ENCRYPTION_KEY" "INTERNAL_AUTH_KEYS") }}{{- end }}
{{- if or (eq .role "knowledge") (eq .role "knowledgeWorker") }}{{- $keys = append $keys "INTERNAL_AUTH_KEYS" }}{{- if not .root.Values.data.objectStore.enabled }}{{- $keys = append $keys "KNOWLEDGE_STORAGE__SIGNING_KEY" }}{{- end }}{{- end }}
{{- $keys | toJson }}
{{- end -}}
{{- define "ai-gateway.appSecretEnv" -}}
{{- if eq .root.Values.workloadIdentity.mode "disabled" }}
{{- include "ai-gateway.secretEnv" . }}
{{- else if and (ne .env "AI_PLATFORM_CAPABILITY_PROOF_SECRET") (ne .env "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN") (not (and (eq .root.Values.workloadIdentity.mode "required") (eq .env "AI_PLATFORM_INTERNAL_TOKEN"))) }}
- name: {{ .env }}_FILE
  value: /run/app-secrets/{{ .env }}
{{- end }}
{{- end -}}
{{- define "ai-gateway.appSecretMount" -}}
{{- if and (ne .root.Values.workloadIdentity.mode "disabled") (gt (len (include "ai-gateway.appSecretKeys" . | fromJsonArray)) 0) }}
- {name: app-secret-private, mountPath: /run/app-secrets, readOnly: true}
{{- end }}
{{- end -}}
{{- define "ai-gateway.appSecretVolume" -}}
{{- if and (ne .root.Values.workloadIdentity.mode "disabled") (gt (len (include "ai-gateway.appSecretKeys" . | fromJsonArray)) 0) }}
{{- $role := ternary "knowledge" .role (eq .role "knowledgeWorker") }}
- name: app-secret-private
  emptyDir: {medium: Memory, sizeLimit: 1Mi}
- name: app-secret-files
  secret:
    secretName: {{ index .root.Values.credentials $role | quote }}
    defaultMode: 0440
    items:
{{- range $key := include "ai-gateway.appSecretKeys" . | fromJsonArray }}
    - {key: {{ $key }}, path: {{ $key }}}
{{- end }}
{{- end }}
{{- end -}}

{{- define "ai-gateway.appSecretInit" -}}
{{- if and (ne .root.Values.workloadIdentity.mode "disabled") (gt (len (include "ai-gateway.appSecretKeys" . | fromJsonArray)) 0) }}
- name: private-application-files
  image: {{ include "ai-gateway.image" (dict "root" .root "unit" "tools") | quote }}
  command: [/bin/sh, -ec]
  args:
  - {{ printf "umask 077; for key in %s; do test -s /input/\"$key\"; cp /input/\"$key\" /output/\"$key\"; chmod 400 /output/\"$key\"; done" (join " " (include "ai-gateway.appSecretKeys" . | fromJsonArray)) | quote }}
  securityContext:
{{ include "ai-gateway.security" .uid | indent 4 }}
  resources: {requests: {cpu: 10m, memory: 16Mi}, limits: {cpu: 100m, memory: 32Mi}}
  volumeMounts:
  - {name: app-secret-files, mountPath: /input, readOnly: true}
  - {name: app-secret-private, mountPath: /output}
{{- end }}
{{- end -}}


{{/* Dataset-only rollout: pins belong under Gateway resources, not capabilities. */}}
{{- define "ai-gateway.datasetAuthorizationEnv" -}}
{{- $configuration := .root.Values.datasetAuthorization -}}
{{- if ne $configuration.mode "off" }}
{{- if eq .service "gateway" }}
{{- $resources := dict "enforce" (ternary (list "dataset") (list) (eq $configuration.mode "enforce")) "expected_policy_sha256" $configuration.expectedPolicySha256 "expected_matrix_sha256" $configuration.expectedMatrixSha256 }}
- name: GATEWAY_CEDAR_CONFIGURATION
  value: {{ dict "mode" $configuration.mode "resources" $resources | toJson | quote }}
{{- else }}
- name: KNOWLEDGE_CEDAR_DATASET_CONFIGURATION
  value: {{ dict "origin" (include "ai-gateway.internalOrigin" (dict "root" .root "service" "gateway")) "mode" $configuration.mode "expected_policy_sha256" $configuration.expectedPolicySha256 "expected_matrix_sha256" $configuration.expectedMatrixSha256 | toJson | quote }}
{{- end }}
{{- end }}
{{- end -}}
