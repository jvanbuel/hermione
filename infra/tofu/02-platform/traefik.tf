locals {
  ingress_ip = local.cluster.ingress_ip

  # sslip.io resolves <anything>.<ip>.sslip.io to <ip>, so a single derived base
  # gives working hostnames for the app, gRPC and Argo CD with no DNS of your
  # own. Setting var.domain later swaps all three at once.
  base_domain = var.domain != "" ? var.domain : "${local.ingress_ip}.sslip.io"

  app_host    = local.base_domain
  grpc_host   = "grpc.${local.base_domain}"
  argocd_host = "argocd.${local.base_domain}"
}

resource "kubernetes_namespace" "traefik" {
  metadata {
    name = "traefik"
  }
}

resource "helm_release" "traefik" {
  name       = "traefik"
  namespace  = kubernetes_namespace.traefik.metadata[0].name
  repository = "https://traefik.github.io/charts"
  chart      = "traefik"
  version    = var.traefik_chart_version

  # Traefik owns the ACME account key on a ReadWriteOnce volume, so it cannot be
  # rolled with two pods alive at once.
  atomic  = true
  timeout = 900

  values = [yamlencode({
    deployment = {
      replicas = 1
    }
    updateStrategy = {
      type = "Recreate"
    }

    # acme.json must survive restarts or Traefik re-issues certificates on every
    # rollout and walks straight into Let's Encrypt's rate limits.
    persistence = {
      enabled      = true
      path         = "/data"
      size         = "128Mi"
      storageClass = "scw-bssd"
    }
    podSecurityContext = {
      fsGroup             = 65532
      fsGroupChangePolicy = "OnRootMismatch"
    }

    certificatesResolvers = {
      letsencrypt = {
        acme = {
          email         = var.acme_email
          caServer      = var.acme_ca_server
          storage       = "/data/acme.json"
          httpChallenge = { entryPoint = "web" }
        }
      }
    }

    ports = {
      web = {
        http = {
          redirections = {
            entryPoint = {
              to        = "websecure"
              scheme    = "https"
              permanent = true
            }
          }
        }
      }
      websecure = {
        http = {
          tls = {
            enabled      = true
            certResolver = "letsencrypt"
          }
        }

        # Hermione is almost entirely long-lived streams: SSE terminal replay,
        # the /ws broadcast socket, and gRPC ingest that stays open for a whole
        # student session. Traefik's default 180s idle timeout would sever all
        # three, so the responding timeouts on this entrypoint are disabled.
        transport = {
          respondingTimeouts = {
            readTimeout  = 0
            writeTimeout = 0
            idleTimeout  = 0
          }
        }
      }
    }

    service = {
      annotations = {
        # Adopt the IP reserved in stage 01. This annotation takes bare UUIDs
        # (not addresses) and supersedes the deprecated spec.loadBalancerIP.
        "service.beta.kubernetes.io/scw-loadbalancer-ip-ids" = local.cluster.ingress_ip_id
        "service.beta.kubernetes.io/scw-loadbalancer-zone"   = local.cluster.zone
        "service.beta.kubernetes.io/scw-loadbalancer-type"   = "LB-S"

        # The load balancer's own timeouts sit in front of Traefik's. Its
        # default 10m tunnel timeout is what silently drops idle WebSocket and
        # gRPC streams, so it is raised well past any plausible lesson.
        "service.beta.kubernetes.io/scw-loadbalancer-timeout-tunnel" = "4h"
        "service.beta.kubernetes.io/scw-loadbalancer-timeout-client" = "30m"

        # Leave live viewers connected when a backend is briefly marked down.
        "service.beta.kubernetes.io/scw-loadbalancer-on-marked-down-action" = "on_marked_down_action_none"

        # Makes the Service report a hostname rather than the raw IP, which is
        # what lets in-cluster clients and the HTTP-01 self-check resolve the
        # public name instead of hairpinning on the address.
        "service.beta.kubernetes.io/scw-loadbalancer-use-hostname" = "true"
      }
      spec = {
        # Cluster (not Local) because Traefik runs a single replica: with Local,
        # every node without that pod would fail the load balancer health check.
        externalTrafficPolicy = "Cluster"
      }
    }

    providers = {
      kubernetesCRD     = { enabled = true }
      kubernetesIngress = { enabled = true }
    }

    log = {
      level = "INFO"
    }
    accessLog = {
      enabled = true
    }
  })]
}
