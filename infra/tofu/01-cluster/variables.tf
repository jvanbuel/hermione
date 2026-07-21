variable "name" {
  description = "Name prefix for every resource in this stack."
  type        = string
  default     = "hermione"
}

variable "region" {
  description = "Scaleway region. Managed Databases can only attach to a Private Network in the region's default AZ, so keep `zone` inside this region."
  type        = string
  default     = "fr-par"
}

variable "zone" {
  description = "Scaleway zone for zoned resources (node pool, load balancer IP). Must be the region's default AZ for the RDB private endpoint to work: fr-par-1, nl-ams-1 or pl-waw-1."
  type        = string
  default     = "fr-par-1"
}

variable "private_network_subnet" {
  description = "IPv4 subnet for the VPC private network shared by the cluster and the database."
  type        = string
  default     = "172.16.32.0/22"
}

# ---------------------------------------------------------------------------
# Kubernetes
# ---------------------------------------------------------------------------

variable "kubernetes_version" {
  description = "Kapsule control plane minor version, as `x.y` (e.g. \"1.36\"). Leave null to track the latest minor Scaleway offers. Patch versions are rejected: auto-upgrade owns the patch level."
  type        = string
  default     = null

  validation {
    condition     = var.kubernetes_version == null || can(regex("^\\d+\\.\\d+$", var.kubernetes_version))
    error_message = "kubernetes_version must be a minor version like \"1.36\", not a patch version like \"1.36.1\" — Scaleway rejects a patch version while auto_upgrade is enabled."
  }
}

variable "node_type" {
  description = "Instance type for worker nodes. DEV1-S / PLAY2-PICO / STARDUST are rejected by Kapsule (too little RAM)."
  type        = string
  default     = "PRO2-XXS"
}

variable "node_pool_min_size" {
  description = "Minimum worker nodes. Two lets Traefik and Hermione survive a single node being drained during an upgrade."
  type        = number
  default     = 2
}

variable "node_pool_max_size" {
  description = "Maximum worker nodes the autoscaler may add."
  type        = number
  default     = 4
}

variable "api_server_allowed_cidrs" {
  description = <<-EOT
    CIDRs allowed to reach the Kubernetes API server. Kapsule defaults to
    0.0.0.0/0; narrowing this is strongly recommended, but whatever machine runs
    `tofu apply` for stage 02 must be inside the list or the apply will hang.
  EOT
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

# ---------------------------------------------------------------------------
# Database
# ---------------------------------------------------------------------------

variable "db_node_type" {
  description = "Managed Database node type. db-dev-s is the cheapest usable tier; the db-pop2-* family is the production line (and requires an sbs_* volume_type)."
  type        = string
  default     = "db-dev-s"
}

variable "db_engine" {
  description = "Managed Database engine. Hermione's migrations are raw Postgres DDL (UUID, BIGSERIAL, TIMESTAMPTZ), so this must be PostgreSQL."
  type        = string
  default     = "PostgreSQL-16"

  validation {
    condition     = startswith(var.db_engine, "PostgreSQL-")
    error_message = "Hermione's SeaORM migrations are Postgres-specific; db_engine must be a PostgreSQL-* version."
  }
}

variable "db_is_ha_cluster" {
  description = "Run the database as an HA pair. Not supported on db-dev-* tiers, and toggling it recreates the instance."
  type        = bool
  default     = false
}

variable "db_backup_retention_days" {
  description = "How many days of automatic backups to keep."
  type        = number
  default     = 7
}

variable "db_sslmode" {
  description = "sslmode for the connection string handed to hermione-server. The database is only reachable over the private network, but `require` still encrypts the hop. Drop to `disable` if the server cannot connect."
  type        = string
  default     = "require"
}

# ---------------------------------------------------------------------------
# Lifecycle
# ---------------------------------------------------------------------------

variable "delete_additional_resources" {
  description = "On cluster destroy, also delete block volumes and the load balancers the CCM created. Leave false in production so a `tofu destroy` cannot silently take data with it."
  type        = bool
  default     = false
}
