resource "kubernetes_namespace" "argocd" {
  metadata {
    name = "argocd"
  }
}

# The Application and the Argo CD IngressRoute are shipped as `extraObjects` of
# this release rather than as kubernetes_manifest resources. kubernetes_manifest
# validates against the live API during *plan*, and neither the Argo CD nor the
# Traefik CRDs exist at that point on a fresh cluster. Helm applies them after
# the CRDs, so the ordering problem disappears.
resource "helm_release" "argocd" {
  name       = "argocd"
  namespace  = kubernetes_namespace.argocd.metadata[0].name
  repository = "https://argoproj.github.io/argo-helm"
  chart      = "argo-cd"
  version    = var.argocd_chart_version

  atomic  = true
  timeout = 900

  depends_on = [
    helm_release.traefik,       # Traefik CRDs must exist for the IngressRoute
    kubernetes_secret.hermione, # the Application's pods read this on first sync
  ]

  values = [yamlencode({
    global = {
      domain = local.argocd_host
    }

    configs = {
      params = {
        # Traefik terminates TLS, so the API server should speak plain HTTP
        # rather than redirect and produce a loop.
        "server.insecure" = true
      }
    }

    server = {
      # Handled by the IngressRoute below instead.
      ingress = { enabled = false }
    }

    extraObjects = [
      {
        apiVersion = "traefik.io/v1alpha1"
        kind       = "IngressRoute"
        metadata = {
          name      = "argocd-server"
          namespace = kubernetes_namespace.argocd.metadata[0].name
        }
        spec = {
          entryPoints = ["websecure"]
          routes = [{
            match = "Host(`${local.argocd_host}`)"
            kind  = "Rule"
            services = [{
              name = "argocd-server"
              port = 80
            }]
          }]
          tls = { certResolver = "letsencrypt" }
        }
      },
      {
        apiVersion = "argoproj.io/v1alpha1"
        kind       = "Application"
        metadata = {
          name       = "hermione"
          namespace  = kubernetes_namespace.argocd.metadata[0].name
          finalizers = ["resources-finalizer.argocd.argoproj.io"]
        }
        spec = {
          project = "default"

          source = {
            repoURL        = var.git_repo_url
            targetRevision = var.git_target_revision
            path           = var.chart_path

            helm = {
              # Values that depend on infrastructure Argo CD cannot know about
              # (the load balancer IP, the Secret's name) are injected here.
              # Everything else lives in the chart's values.yaml, in git.
              valuesObject = {
                image = {
                  repository = var.image_repository
                  tag        = var.image_tag
                }

                existingSecret = kubernetes_secret.hermione.metadata[0].name

                assistant = {
                  model = var.assistant_model
                }

                ingress = {
                  enabled      = true
                  domain       = local.app_host
                  grpcDomain   = local.grpc_host
                  certResolver = "letsencrypt"
                }
              }
            }
          }

          destination = {
            server    = "https://kubernetes.default.svc"
            namespace = kubernetes_namespace.hermione.metadata[0].name
          }

          syncPolicy = {
            automated = {
              prune    = true
              selfHeal = true
            }
            # The namespace is created by OpenTofu (it has to exist before the
            # Secret above), so Argo CD must not try to own it.
            syncOptions = ["CreateNamespace=false"]
          }
        }
      },
    ]
  })]
}
