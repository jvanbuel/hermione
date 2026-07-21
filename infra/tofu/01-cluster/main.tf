# ---------------------------------------------------------------------------
# Network
#
# One VPC private network carries both the Kubernetes nodes and the database's
# private endpoint, so Postgres is never exposed to the internet.
# ---------------------------------------------------------------------------

resource "scaleway_vpc" "main" {
  name   = var.name
  region = var.region
}

resource "scaleway_vpc_private_network" "main" {
  name   = "${var.name}-pn"
  vpc_id = scaleway_vpc.main.id
  region = var.region

  ipv4_subnet {
    subnet = var.private_network_subnet
  }
}

# ---------------------------------------------------------------------------
# Kubernetes (Kapsule)
# ---------------------------------------------------------------------------

data "scaleway_k8s_version" "latest" {
  name   = "latest"
  region = var.region
}

locals {
  # The data source returns a full x.y.z (e.g. "1.36.1"), but Scaleway rejects a
  # patch version when auto_upgrade is enabled — the whole point of auto-upgrade
  # is that it owns the patch level — so track the minor version only.
  kubernetes_version = coalesce(
    var.kubernetes_version,
    join(".", slice(split(".", data.scaleway_k8s_version.latest.name), 0, 2)),
  )
}

resource "scaleway_k8s_cluster" "main" {
  name               = var.name
  region             = var.region
  type               = "kapsule"
  version            = local.kubernetes_version
  cni                = "cilium"
  private_network_id = scaleway_vpc_private_network.main.id

  # Whether destroying the cluster also destroys CCM-created load balancers and
  # block volumes. See the variable's description before flipping this.
  delete_additional_resources = var.delete_additional_resources

  auto_upgrade {
    enable                        = true
    maintenance_window_start_hour = 3
    maintenance_window_day        = "sunday"
  }

  autoscaler_config {
    balance_similar_node_groups   = true
    ignore_daemonsets_utilization = true
    scale_down_delay_after_add    = "10m"
    scale_down_unneeded_time      = "10m"
  }
}

resource "scaleway_k8s_pool" "main" {
  cluster_id = scaleway_k8s_cluster.main.id
  name       = "workers"
  zone       = var.zone

  node_type         = var.node_type
  size              = var.node_pool_min_size
  min_size          = var.node_pool_min_size
  max_size          = var.node_pool_max_size
  autoscaling       = true
  autohealing       = true
  container_runtime = "containerd"

  # Stage 02 talks to the API server immediately after this, so the pool must
  # actually be Ready before this resource is considered created.
  wait_for_pool_ready = true
}

resource "scaleway_k8s_acl" "api_server" {
  cluster_id = scaleway_k8s_cluster.main.id
  region     = var.region

  dynamic "acl_rules" {
    for_each = var.api_server_allowed_cidrs
    content {
      ip          = acl_rules.value
      description = "allowed operator network"
    }
  }
}

# ---------------------------------------------------------------------------
# Ingress IP
#
# Reserved here rather than left to the CCM so the address — and therefore the
# sslip.io hostnames built from it — are known before Traefik is installed.
# The Scaleway CCM adopts it via a Service annotation in stage 02.
# ---------------------------------------------------------------------------

resource "scaleway_lb_ip" "ingress" {
  zone = var.zone
}
