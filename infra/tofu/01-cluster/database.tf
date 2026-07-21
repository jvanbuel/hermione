# ---------------------------------------------------------------------------
# Managed PostgreSQL
#
# hermione-server runs its SeaORM migrations in-process on every start
# (crates/server/src/main.rs), so the application user needs DDL rights on its
# own database — hence `permission = "all"` rather than readwrite.
# ---------------------------------------------------------------------------

# Scaleway rejects a database password that lacks a digit, an uppercase, a
# lowercase and a special character. The special set is restricted to characters
# that need no percent-encoding in the connection URL, so the generated DSN
# stays valid without escaping.
resource "random_password" "db_admin" {
  length           = 32
  min_upper        = 2
  min_lower        = 2
  min_numeric      = 2
  min_special      = 2
  override_special = "!*-_.~"
}

resource "random_password" "db_app" {
  length           = 32
  min_upper        = 2
  min_lower        = 2
  min_numeric      = 2
  min_special      = 2
  override_special = "!*-_.~"
}

resource "scaleway_rdb_instance" "main" {
  name          = var.name
  region        = var.region
  node_type     = var.db_node_type
  engine        = var.db_engine
  is_ha_cluster = var.db_is_ha_cluster

  user_name = "hermione_admin"
  password  = random_password.db_admin.result

  disable_backup            = false
  backup_schedule_frequency = 24
  backup_schedule_retention = var.db_backup_retention_days
  backup_same_region        = true
  encryption_at_rest        = true

  # Attaching a private endpoint with IPAM enabled is what registers the
  # instance in VPC DNS. Do NOT switch this to `ip_net`: that provisions the
  # endpoint in static mode, which is not registered in IPAM or DNS and leaves
  # pods unable to resolve or route to it.
  private_network {
    pn_id       = scaleway_vpc_private_network.main.id
    enable_ipam = true
  }

  # Deliberately no `load_balancer {}` block: omitting it means no public
  # endpoint is created, so the database is reachable only from the VPC.

  lifecycle {
    # Losing the database to an accidental `tofu destroy` is unrecoverable.
    prevent_destroy = true
  }
}

resource "scaleway_rdb_database" "hermione" {
  instance_id = scaleway_rdb_instance.main.id
  name        = "hermione"
}

resource "scaleway_rdb_user" "hermione" {
  instance_id = scaleway_rdb_instance.main.id
  name        = "hermione"
  password    = random_password.db_app.result
  is_admin    = false
}

resource "scaleway_rdb_privilege" "hermione" {
  instance_id   = scaleway_rdb_instance.main.id
  database_name = scaleway_rdb_database.hermione.name
  user_name     = scaleway_rdb_user.hermione.name
  permission    = "all"
}

locals {
  db_endpoint = scaleway_rdb_instance.main.private_network[0]

  # Prefer the IPAM-registered hostname so the DSN survives the endpoint's IP
  # changing; fall back to the address if Scaleway did not return one.
  db_host = coalesce(local.db_endpoint.hostname, local.db_endpoint.ip)

  database_url = format(
    "postgres://%s:%s@%s:%d/%s?sslmode=%s",
    scaleway_rdb_user.hermione.name,
    random_password.db_app.result,
    local.db_host,
    local.db_endpoint.port,
    scaleway_rdb_database.hermione.name,
    var.db_sslmode,
  )
}
