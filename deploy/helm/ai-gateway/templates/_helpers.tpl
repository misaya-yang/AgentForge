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
{{- include "ai-gateway.secretEnv" (dict "env" .env "secret" .root.Values.databaseRoles.existingSecret "key" .key) -}}
{{- end -}}
{{- define "ai-gateway.envMap" -}}
{{- range $key, $value := . }}
- name: {{ $key }}
  value: {{ $value | toString | quote }}
{{- end -}}
{{- end -}}
{{- define "ai-gateway.validate" -}}
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
