variable "domain" {
  description = <<-EOT
    Base domain for Hermione. Leave empty to derive one from the reserved load
    balancer IP via sslip.io (e.g. "51.15.0.1.sslip.io"), which resolves without
    you owning any DNS and still satisfies Let's Encrypt's HTTP-01 challenge.
    When you have a real domain, point it at the ingress IP and set it here —
    every hostname below follows automatically.
  EOT
  type        = string
  default     = ""
}

variable "acme_email" {
  description = "Contact address for the Let's Encrypt account Traefik registers. Expiry warnings go here."
  type        = string
}

variable "acme_ca_server" {
  description = "ACME directory URL. Point at the staging endpoint while iterating so you cannot exhaust Let's Encrypt's rate limits on a hostname."
  type        = string
  default     = "https://acme-v02.api.letsencrypt.org/directory"
}

# ---------------------------------------------------------------------------
# GitOps
# ---------------------------------------------------------------------------

variable "git_repo_url" {
  description = "Repository Argo CD reconciles the Hermione chart from."
  type        = string
  default     = "https://github.com/jvanbuel/hermione.git"
}

variable "git_target_revision" {
  description = "Branch, tag or commit Argo CD tracks. A branch means a push deploys; a tag means deploys are explicit."
  type        = string
  default     = "main"
}

variable "chart_path" {
  description = "Path to the Hermione chart inside the repository."
  type        = string
  default     = "infra/charts/hermione"
}

variable "image_repository" {
  description = "Container image for hermione-server. Built and pushed by .github/workflows/release.yml on v* tags."
  type        = string
  default     = "ghcr.io/jvanbuel/hermione"
}

variable "image_tag" {
  description = "Image tag to deploy. Prefer a real version tag over `latest` so Argo CD has something meaningful to diff."
  type        = string
  default     = "latest"
}

# ---------------------------------------------------------------------------
# Application secrets
# ---------------------------------------------------------------------------

variable "bootstrap_admin_username" {
  description = "Username of the admin account seeded on the server's first start."
  type        = string
  default     = "admin"
}

variable "bootstrap_admin_password" {
  description = "Password for the seeded admin account. Leave empty to have one generated; read it back with `make admin-password`."
  type        = string
  default     = ""
  sensitive   = true
}

variable "anthropic_api_key" {
  description = "Anthropic API key enabling the per-course AI teaching assistant. Leave empty to keep the assistant disabled; the key is then omitted from the Secret entirely rather than set to an empty string."
  type        = string
  default     = ""
  sensitive   = true
}

variable "assistant_model" {
  description = "Default model for course assistants (HERMIONE_ASSISTANT_MODEL)."
  type        = string
  default     = "claude-opus-4-8"
}

variable "oidc_providers" {
  description = <<-EOT
    JSON array configuring verified student identity, e.g.
    [{"name":"github","kind":"github","clientId":"..."}].
    Leave empty to keep identity as self-asserted attribution. Note that once
    this is set, Hermione *enforces* verified identity on ingest.
  EOT
  type        = string
  default     = ""
  sensitive   = true
}

# ---------------------------------------------------------------------------
# Chart versions — pinned so a re-apply months from now installs what you tested
# ---------------------------------------------------------------------------

variable "traefik_chart_version" {
  type        = string
  description = "Version of the traefik Helm chart."
  default     = "41.0.2"
}

variable "argocd_chart_version" {
  type        = string
  description = "Version of the argo-cd Helm chart."
  default     = "10.1.4"
}
