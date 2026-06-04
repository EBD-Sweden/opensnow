{{/*
Expand the name of the chart.
*/}}
{{- define "opensnow.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
*/}}
{{- define "opensnow.fullname" -}}
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
Create chart name and version as used by the chart label.
*/}}
{{- define "opensnow.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Common labels
*/}}
{{- define "opensnow.labels" -}}
helm.sh/chart: {{ include "opensnow.chart" . }}
{{ include "opensnow.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "opensnow.selectorLabels" -}}
app.kubernetes.io/name: {{ include "opensnow.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Coordinator labels
*/}}
{{- define "opensnow.coordinator.labels" -}}
{{ include "opensnow.labels" . }}
app.kubernetes.io/component: coordinator
{{- end }}

{{- define "opensnow.coordinator.selectorLabels" -}}
{{ include "opensnow.selectorLabels" . }}
app.kubernetes.io/component: coordinator
{{- end }}

{{/*
Worker labels
*/}}
{{- define "opensnow.worker.labels" -}}
{{ include "opensnow.labels" . }}
app.kubernetes.io/component: worker
{{- end }}

{{- define "opensnow.worker.selectorLabels" -}}
{{ include "opensnow.selectorLabels" . }}
app.kubernetes.io/component: worker
{{- end }}

{{/*
Metadata labels
*/}}
{{- define "opensnow.metadata.labels" -}}
{{ include "opensnow.labels" . }}
app.kubernetes.io/component: metadata
{{- end }}

{{- define "opensnow.metadata.selectorLabels" -}}
{{ include "opensnow.selectorLabels" . }}
app.kubernetes.io/component: metadata
{{- end }}

{{/*
Service account name
*/}}
{{- define "opensnow.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "opensnow.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/*
Metadata connection DSN
*/}}
{{- define "opensnow.metadata.dsn" -}}
{{- if .Values.metadata.external.enabled }}
{{- printf "postgres://%s:$(OPENSNOW_METADATA_PASSWORD)@%s:%d/%s?sslmode=require" .Values.metadata.external.username .Values.metadata.external.host (int .Values.metadata.external.port) .Values.metadata.external.database }}
{{- else }}
{{- printf "postgres://%s:$(OPENSNOW_METADATA_PASSWORD)@%s-metadata:%d/%s?sslmode=disable" .Values.metadata.builtin.username (include "opensnow.fullname" .) (int .Values.metadata.builtin.port) .Values.metadata.builtin.database }}
{{- end }}
{{- end }}

{{/*
Image reference
*/}}
{{- define "opensnow.image" -}}
{{- if .Values.global.imageRegistry }}
{{- printf "%s/%s:%s" .Values.global.imageRegistry .Values.image.repository .Values.image.tag }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository .Values.image.tag }}
{{- end }}
{{- end }}
