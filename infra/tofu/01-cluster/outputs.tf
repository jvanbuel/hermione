output "cluster_id" {
  description = "Regional ID of the Kapsule cluster (region/uuid)."
  value       = scaleway_k8s_cluster.main.id
}

output "kubeconfig" {
  description = "Raw kubeconfig for the cluster. Write it to a file to use kubectl directly."
  value       = scaleway_k8s_cluster.main.kubeconfig[0].config_file
  sensitive   = true
}

output "kubernetes_host" {
  description = "API server URL, for the kubernetes/helm providers in stage 02."
  value       = scaleway_k8s_cluster.main.kubeconfig[0].host
  # The whole kubeconfig block is sensitive, so this derived value inherits it.
  sensitive = true
}

output "kubernetes_token" {
  description = "API server token, for the kubernetes/helm providers in stage 02."
  value       = scaleway_k8s_cluster.main.kubeconfig[0].token
  sensitive   = true
}

output "kubernetes_cluster_ca_certificate" {
  description = "Base64-encoded cluster CA, for the kubernetes/helm providers in stage 02."
  value       = scaleway_k8s_cluster.main.kubeconfig[0].cluster_ca_certificate
  sensitive   = true
}

output "ingress_ip" {
  description = "The reserved load balancer IP that Traefik's Service will adopt."
  value       = scaleway_lb_ip.ingress.ip_address
}

output "ingress_ip_id" {
  description = "Bare UUID of the reserved IP. The CCM's scw-loadbalancer-ip-ids annotation wants the UUID without the zone prefix."
  value       = split("/", scaleway_lb_ip.ingress.id)[1]
}

output "zone" {
  description = "Zone the ingress IP lives in; the CCM needs it to place the load balancer."
  value       = var.zone
}

output "database_url" {
  description = "Postgres DSN for HERMIONE_DATABASE_URL, pointing at the private endpoint."
  value       = local.database_url
  sensitive   = true
}

output "database_host" {
  description = "Private hostname of the database endpoint."
  value       = local.db_host
}
