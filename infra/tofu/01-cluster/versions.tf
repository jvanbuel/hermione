terraform {
  required_version = ">= 1.9"

  required_providers {
    scaleway = {
      source  = "scaleway/scaleway"
      version = "~> 2.79"
    }
    random = {
      source  = "hashicorp/random"
      version = "~> 3.6"
    }
  }
}

# Credentials come from the environment (SCW_ACCESS_KEY / SCW_SECRET_KEY /
# SCW_DEFAULT_PROJECT_ID), which `scw init` writes to ~/.config/scw/config.yaml
# and the provider reads directly.
provider "scaleway" {
  region = var.region
  zone   = var.zone
}
