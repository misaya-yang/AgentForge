{{/*
Expand the name of the chart.
*/}}
{{- define "ai-gateway.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "ai-gateway.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "ai-gateway.labels" -}}
helm.sh/chart: {{ include "ai-gateway.name" . }}-{{ .Chart.Version | replace "+" "_" }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
app.kubernetes.io/part-of: ai-gateway
{{- end }}

{{/*
Selector labels for a component
*/}}
{{- define "ai-gateway.selectorLabels" -}}
app.kubernetes.io/name: {{ include "ai-gateway.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
PostgreSQL host - internal or external
*/}}
{{- define "ai-gateway.postgresHost" -}}
{{- if .Values.postgresql.enabled }}
{{- printf "%s-postgresql" (include "ai-gateway.fullname" .) }}
{{- else }}
{{- .Values.externalDatabase.host }}
{{- end }}
{{- end }}

{{/*
PostgreSQL port
*/}}
{{- define "ai-gateway.postgresPort" -}}
{{- if .Values.postgresql.enabled }}
{{- "5432" }}
{{- else }}
{{- .Values.externalDatabase.port | toString }}
{{- end }}
{{- end }}

{{/*
PostgreSQL database name
*/}}
{{- define "ai-gateway.postgresDatabase" -}}
{{- if .Values.postgresql.enabled }}
{{- "gateway" }}
{{- else }}
{{- .Values.externalDatabase.database }}
{{- end }}
{{- end }}

{{/*
PostgreSQL user
*/}}
{{- define "ai-gateway.postgresUser" -}}
{{- if .Values.postgresql.enabled }}
{{- "postgres" }}
{{- else }}
{{- .Values.externalDatabase.user }}
{{- end }}
{{- end }}

{{/*
Redis host - internal or external
*/}}
{{- define "ai-gateway.redisHost" -}}
{{- if .Values.redis.enabled }}
{{- printf "%s-redis" (include "ai-gateway.fullname" .) }}
{{- else }}
{{- .Values.externalRedis.host }}
{{- end }}
{{- end }}

{{/*
Redis port
*/}}
{{- define "ai-gateway.redisPort" -}}
{{- if .Values.redis.enabled }}
{{- "6379" }}
{{- else }}
{{- .Values.externalRedis.port | toString }}
{{- end }}
{{- end }}

{{/*
Qdrant URL - internal or external
*/}}
{{- define "ai-gateway.qdrantUrl" -}}
{{- if .Values.qdrant.enabled }}
{{- printf "http://%s-qdrant:6333" (include "ai-gateway.fullname" .) }}
{{- else }}
{{- .Values.externalQdrant.url }}
{{- end }}
{{- end }}

{{/*
Full PostgreSQL DSN
*/}}
{{- define "ai-gateway.postgresDSN" -}}
postgresql://$(POSTGRES_USER):$(POSTGRES_PASSWORD)@{{ include "ai-gateway.postgresHost" . }}:{{ include "ai-gateway.postgresPort" . }}/{{ include "ai-gateway.postgresDatabase" . }}
{{- end }}

{{/*
Full Redis URL
*/}}
{{- define "ai-gateway.redisURL" -}}
redis://:$(REDIS_PASSWORD)@{{ include "ai-gateway.redisHost" . }}:{{ include "ai-gateway.redisPort" . }}/0
{{- end }}

{{/*
Knowledge Service URL (internal)
*/}}
{{- define "ai-gateway.knowledgeServiceUrl" -}}
{{- if .Values.knowledgeService.enabled }}
{{- printf "http://%s-knowledge:8092" (include "ai-gateway.fullname" .) }}
{{- end }}
{{- end }}

{{/*
LangGraph instance URLs — comma-separated list of all enabled agent service URLs.
Used by the gateway to discover and proxy LangGraph agents.
*/}}
{{- define "ai-gateway.langgraphInstanceURLs" -}}
{{- $urls := list }}
{{- if .Values.langgraph.enabled }}
{{- range $name, $agent := .Values.langgraph.agents }}
{{- if $agent.enabled }}
{{- $urls = append $urls (printf "http://%s-langgraph-%s:%s" (include "ai-gateway.fullname" $) $name ($agent.port | toString)) }}
{{- end }}
{{- end }}
{{- end }}
{{- join "," $urls }}
{{- end }}
