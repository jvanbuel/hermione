# ---------------------------------------------------------------------------
# Hermione namespace and secrets
#
# Secrets are created here rather than committed to git: the chart only ever
# references them by name, so nothing sensitive is in the repository Argo CD
# reconciles from. The trade-off is that these values live in OpenTofu state —
# keep that state private, or move to Scaleway Secret Manager + the External
# Secrets Operator when more than one person operates this.
# ---------------------------------------------------------------------------

resource "kubernetes_namespace" "hermione" {
  metadata {
    name = "hermione"
  }
}

# Guards the provisioning API (/api/admin/*) and the gRPC Viewer service.
#
# This is not optional. Until an admin account exists, Hermione runs in "open
# dev mode": the dashboard is fully unauthenticated and scoped to a seeded
# `default` course. Deploying without this token — and without then creating an
# admin with it — publishes every student's terminal to the internet.
resource "random_password" "admin_token" {
  length  = 48
  special = false
}

# HS256 signing key for the short-lived identity tokens Hermione issues when
# OIDC is configured. Rotating it invalidates outstanding student tokens.
resource "random_password" "identity_secret" {
  length  = 64
  special = false
}

# Password for the admin account the server seeds on its first start, which is
# what stops a fresh deployment from serving an unauthenticated dashboard.
# Generated unless you supply one. No special characters: this gets typed into
# a login form and pasted between terminals.
resource "random_password" "bootstrap_admin" {
  length  = 24
  special = false
}

locals {
  bootstrap_admin_password = var.bootstrap_admin_password != "" ? var.bootstrap_admin_password : random_password.bootstrap_admin.result
}

resource "kubernetes_secret" "hermione" {
  metadata {
    name      = "hermione-secrets"
    namespace = kubernetes_namespace.hermione.metadata[0].name
  }

  type = "Opaque"

  # Consumed with envFrom, so each key becomes an environment variable directly.
  # Optional entries are omitted rather than set empty: hermione-server treats a
  # present-but-empty HERMIONE_ANTHROPIC_API_KEY as "assistant enabled" and then
  # fails every call, whereas an absent variable cleanly disables the feature.
  data = merge(
    {
      HERMIONE_DATABASE_URL    = local.cluster.database_url
      HERMIONE_ADMIN_TOKEN     = random_password.admin_token.result
      HERMIONE_IDENTITY_SECRET = random_password.identity_secret.result

      # Seeds the first admin account at startup and grants it every existing
      # course. Only applied while the admin table is empty, so it is safe to
      # leave set — later password changes made in the UI are not clobbered.
      HERMIONE_BOOTSTRAP_ADMIN_USERNAME = var.bootstrap_admin_username
      HERMIONE_BOOTSTRAP_ADMIN_PASSWORD = local.bootstrap_admin_password
    },
    var.anthropic_api_key != "" ? {
      HERMIONE_ANTHROPIC_API_KEY = var.anthropic_api_key
    } : {},
    var.oidc_providers != "" ? {
      HERMIONE_OIDC_PROVIDERS = var.oidc_providers
    } : {},
  )
}
