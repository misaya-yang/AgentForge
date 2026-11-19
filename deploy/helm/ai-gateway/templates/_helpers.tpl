{{- define "ai-gateway.fullname" -}}
{{- default (printf "%s-%s" .Release.Name .Chart.Name) .Values.fullnameOverride | trunc 18 | trimSuffix "-" -}}
{{- end -}}
{{- define "ai-gateway.labels" -}}
app.kubernetes.io/name: ai-gateway
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: agentforge
{{- end -}}
{{- define "ai-gateway.image" -}}
{{- $image := index .root.Values.images .unit -}}
{{- if not (regexMatch "^sha256:[0-9a-f]{64}$" $image.digest) -}}{{ fail (printf "images.%s.digest must be an admitted sha256 digest" .unit) }}{{- end -}}
{{- printf "%s@%s" $image.repository $image.digest -}}
{{- end -}}
{{- define "ai-gateway.security" -}}
runAsNonRoot: true
runAsUser: {{ . }}
runAsGroup: {{ . }}
readOnlyRootFilesystem: true
allowPrivilegeEscalation: false
capabilities:
  drop: [ALL]
{{- end -}}
{{- define "ai-gateway.secretEnv" -}}
- name: {{ .env }}
  valueFrom:
    secretKeyRef:
      name: {{ required (printf "Secret reference required for %s" .env) .secret }}
      key: {{ .key }}
{{- end -}}
{{- define "ai-gateway.roleEnv" -}}
{{- if .root.Values.dataSecurity.enabled }}
{{- if hasPrefix "AI_GATEWAY_DATABASE_" .env }}
- name: {{ .env }}_FILE
  value: /run/data/credentials/{{ ternary "authority-dsn" "dsn" (and (eq .root.Values.dataSecurity.mode "session") (ne .key "MIGRATOR_DSN")) }}
{{- $prefix := trimSuffix "_DSN" .env }}
{{- range $suffix, $file := dict "TLS_CA_FILE" "ca-bundle.pem" "TLS_CERT_FILE" "tls.crt" "TLS_KEY_FILE" "tls.key" }}
- name: {{ $prefix }}_{{ $suffix }}
  value: /run/data/credentials/{{ $file }}
{{- end }}
{{- end }}
{{- else }}
{{- include "ai-gateway.secretEnv" (dict "env" .env "secret" .root.Values.databaseRoles.existingSecret "key" .key) -}}
{{- end }}
{{- end -}}
{{- define "ai-gateway.envMap" -}}
{{- range $key, $value := . }}
- name: {{ $key }}
  value: {{ $value | toString | quote }}
{{- end -}}
{{- end -}}
{{- define "ai-gateway.validate" -}}
{{- if .Values.sandbox.strongPool.enabled }}
{{- if .Values.sandbox.enabled }}{{ fail "select one sandbox socket authority: legacy sandbox or strongPool" }}{{- end }}
{{- if not .Values.sandbox.signingSecret }}{{ fail "strongPool requires the Worker's purpose-specific sandbox.signingSecret" }}{{- end }}
{{- if gt .Values.sandbox.strongPool.warm .Values.sandbox.strongPool.maximum }}{{ fail "strongPool warm must not exceed maximum" }}{{- end }}
{{- if or (not (hasPrefix "/" .Values.sandbox.strongPool.sharedHostPath)) (contains ".." .Values.sandbox.strongPool.sharedHostPath) (eq "/" .Values.sandbox.strongPool.sharedHostPath) }}{{ fail "strongPool sharedHostPath must be a dedicated absolute node directory" }}{{- end }}
{{- end }}
{{- if ne .Values.datasetAuthorization.mode "off" }}
{{- if ne .Values.workloadIdentity.mode "required" }}{{ fail "datasetAuthorization requires workloadIdentity.mode=required" }}{{- end }}
{{- if or (not (regexMatch "^[0-9a-f]{64}$" .Values.datasetAuthorization.expectedPolicySha256)) (not (regexMatch "^[0-9a-f]{64}$" .Values.datasetAuthorization.expectedMatrixSha256)) }}{{ fail "datasetAuthorization requires both native resource policy/matrix pins" }}{{- end }}
{{- end }}
{{- if ne .Values.workloadIdentity.mode "disabled" }}
{{- if .Values.routing.internalGrpc.enabled }}{{ fail "workload mTLS requires the native HTTP facade; plaintext internal gRPC must be disabled" }}{{- end }}
{{- if not (regexMatch "^[a-z0-9][a-z0-9.-]*[a-z0-9]$" .Values.workloadIdentity.trustDomain) }}{{ fail "workload trustDomain must be a canonical SPIFFE trust domain" }}{{- end }}
{{- $tools := include "ai-gateway.image" (dict "root" . "unit" "tools") }}
{{- range $role := list "gateway" "runtime" "control" "knowledge" "knowledgeWorker" }}
{{- if not (index $.Values.workloadIdentity.secrets $role) }}{{ fail (printf "workloadIdentity.secrets.%s is required" $role) }}{{- end }}
{{- end }}
{{- if and .Values.sdkWorker.enabled (not .Values.workloadIdentity.secrets.sdk) }}{{ fail "workloadIdentity.secrets.sdk is required" }}{{- end }}
{{- end }}
{{- if .Values.dataSecurity.enabled }}
{{- if ne .Values.databaseRoles.rolePrefix "ai_gateway_" }}{{ fail "data mTLS currently requires canonical ai_gateway_ roles" }}{{- end }}
{{- if eq .Values.dataSecurity.mode "session" }}
{{- if not (and .Values.dataSecurity.directPostgres .Values.dataSecurity.directPostgres.peer.namespaceSelector.matchLabels .Values.dataSecurity.directPostgres.peer.podSelector.matchLabels) }}{{ fail "session pooling requires explicit directPostgres peer for startup authority and cancellation" }}{{- end }}
{{- end }}
{{- $roles := list }}{{- range $role, $secret := .Values.dataSecurity.roles }}{{- $roles = append $roles $secret }}{{- end }}
{{- if ne (len $roles) (len (uniq $roles)) }}{{ fail "database role Secrets must be distinct" }}{{- end }}
{{- if not .Values.dataSecurity.generation }}{{ fail "dataSecurity.generation is required for restart-based database credential rotation" }}{{- end }}
{{- range $role, $secret := .Values.dataSecurity.roles }}
{{- if not $secret }}{{ fail (printf "dataSecurity.roles.%s is required" $role) }}{{- end }}
{{- end }}
{{- end }}
{{- if and (or (eq .Values.global.deploymentMode "multi-instance") (gt (int .Values.knowledgeService.replicaCount) 1) (gt (int .Values.knowledgeWorker.replicaCount) 1) (and .Values.knowledgeService.autoscaling.enabled (gt (int .Values.knowledgeService.autoscaling.maxReplicas) 1)) .Values.knowledgeWorker.keda.enabled) (not .Values.data.objectStore.enabled) -}}
{{- fail "Multi-instance storage requires data.objectStore.enabled and shared S3 credentials" -}}
{{- end -}}
{{- range $unit := list "gateway" "agentRuntime" "capabilityWorker" -}}
{{- $config := index $.Values $unit -}}
{{- if or (ne (int $config.replicaCount) 1) $config.autoscaling.enabled (ne (int $config.autoscaling.minReplicas) 1) (ne (int $config.autoscaling.maxReplicas) 1) -}}{{ fail (printf "%s requires one process owner; autoscaling is not admitted" $unit) }}{{- end -}}
{{- end -}}
{{- $_ := required "databaseRoles.existingSecret must already exist before migration hooks" .Values.databaseRoles.existingSecret -}}
{{- $_ := required "Gateway kernel_revision must be reviewed" .Values.gateway.nativeOptions.kernel_revision -}}
{{- range $key, $credential := .Values.credentials -}}{{- $_ := required (printf "credentials.%s Secret must already exist" $key) $credential -}}{{- end -}}
{{- range $key, $volume := .Values.persistence -}}{{- $_ := required (printf "persistence.%s existingClaim required" $key) $volume.existingClaim -}}{{- end -}}
{{- range $key := list "postgres" "valkey" "qdrant" -}}
{{- $peer := (index $.Values.data $key).peer -}}
{{- if or (empty (dig "namespaceSelector" "matchLabels" (dict) $peer)) (empty (dig "podSelector" "matchLabels" (dict) $peer)) -}}{{ fail (printf "data.%s requires explicit operator namespace/pod selectors" $key) }}{{- end -}}
{{- end -}}
{{- if not (hasPrefix "https://" .Values.data.qdrant.url) -}}{{ fail "Qdrant requires verified HTTPS" }}{{- end -}}
{{- $_ := required "Qdrant client Secret required" .Values.data.qdrant.existingSecret -}}
{{- $_ := required "Complete system+Qdrant CA ConfigMap required" .Values.data.qdrant.caConfigMap -}}
{{- $_ := required "Gateway-only signing Secret required" .Values.lease.gatewayPrivateSecret -}}
{{- $_ := required "Consumer public verifier ConfigMap required" .Values.lease.publicConfigMap -}}
{{- $_ := required "Kubernetes DNS resolver address required for frontend" .Values.network.dnsResolver -}}
{{- if .Values.sandbox.enabled -}}
{{- range $key := list "nodeName" "runtimeClassName" "seccompProfile" "socketClaim" "workspaceClaim" "journalClaim" "signingSecret" "publicKeyId" -}}{{- $_ := required (printf "sandbox.%s required" $key) (index $.Values.sandbox $key) -}}{{- end -}}
{{- if not (regexMatch "^[0-9a-f]{64}$" .Values.sandbox.publicKeyHex) }}{{ fail "sandbox requires public Ed25519 hex only" }}{{ end -}}
{{- if or (not .Values.sandbox.socketFsGroupMutationDisabled) (empty .Values.sandbox.cgroupCSI.driver) -}}{{ fail "sandbox requires prepared node-local socket CSI/PVC with fsGroup mutation disabled and delegated cgroup CSI" }}{{ end -}}
{{- end -}}
{{- end -}}
