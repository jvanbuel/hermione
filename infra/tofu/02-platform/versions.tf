terraform {
  required_version = ">= 1.9"

  required_providers {
    kubernetes = {
      source  = "hashicorp/kubernetes"
      version = "~> 2.35"
    }
    helm = {
      source  = "hashicorp/helm"
      version = "~> 3.0"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.6"
    }
  }
}

# Stage 01 is a separate root module on purpose. The kubernetes and helm
# providers have to be *configured* before OpenTofu can plan any resource that
# belongs to them, and a cluster that does not exist yet cannot supply a host or
# a token. Splitting the apply is what makes `tofu plan` work here at all.
data "terraform_remote_state" "cluster" {
  backend = "local"

  config = {
    path = "${path.module}/../01-cluster/terraform.tfstate"
  }
}

locals {
  cluster = data.terraform_remote_state.cluster.outputs
}

provider "kubernetes" {
  host                   = local.cluster.kubernetes_host
  token                  = local.cluster.kubernetes_token
  cluster_ca_certificate = base64decode(local.cluster.kubernetes_cluster_ca_certificate)
}

provider "helm" {
  kubernetes = {
    host                   = local.cluster.kubernetes_host
    token                  = local.cluster.kubernetes_token
    cluster_ca_certificate = base64decode(local.cluster.kubernetes_cluster_ca_certificate)
  }
}
