CLUSTER  := tofu/01-cluster
PLATFORM := tofu/02-platform

.PHONY: help
help:
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) \
	  | awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-20s\033[0m %s\n", $$1, $$2}'

.PHONY: init
init: ## Initialise both stages
	tofu -chdir=$(CLUSTER) init
	tofu -chdir=$(PLATFORM) init

.PHONY: plan
plan: ## Plan both stages (stage 02 only works once stage 01 is applied)
	tofu -chdir=$(CLUSTER) plan
	tofu -chdir=$(PLATFORM) plan

.PHONY: apply-cluster
apply-cluster: ## Stage 1: VPC, Kapsule, Managed Postgres, reserved ingress IP
	tofu -chdir=$(CLUSTER) apply

.PHONY: apply-platform
apply-platform: ## Stage 2: Traefik, Argo CD, secrets, and the Hermione Application
	tofu -chdir=$(PLATFORM) apply

.PHONY: apply
apply: apply-cluster apply-platform ## Full deploy, in order

.PHONY: kubeconfig
kubeconfig: ## Write the cluster kubeconfig to ./kubeconfig
	@tofu -chdir=$(CLUSTER) output -raw kubeconfig > kubeconfig
	@chmod 600 kubeconfig
	@echo "export KUBECONFIG=$(CURDIR)/kubeconfig"

.PHONY: urls
urls: ## Show the deployed URLs
	@tofu -chdir=$(PLATFORM) output

.PHONY: admin-password
admin-password: ## Print the seeded Hermione admin login
	@echo "username: $$(tofu -chdir=$(PLATFORM) output -raw hermione_admin_username)"
	@echo "password: $$(tofu -chdir=$(PLATFORM) output -raw hermione_admin_password)"

.PHONY: admin-token
admin-token: ## Print Hermione's provisioning token (for the /api/admin/* API)
	@tofu -chdir=$(PLATFORM) output -raw hermione_admin_token; echo

.PHONY: argocd-password
argocd-password: ## Print Argo CD's initial admin password
	@kubectl -n argocd get secret argocd-initial-admin-secret \
	  -o jsonpath='{.data.password}' | base64 -d; echo

.PHONY: lint
lint: ## Format, validate and lint everything
	tofu fmt -recursive tofu
	tofu -chdir=$(CLUSTER) validate
	tofu -chdir=$(PLATFORM) validate
	helm lint charts/hermione \
	  --set ingress.domain=example.com --set ingress.grpcDomain=grpc.example.com

.PHONY: template
template: ## Render the Hermione chart locally
	helm template hermione charts/hermione -n hermione \
	  --set ingress.domain=example.com --set ingress.grpcDomain=grpc.example.com

.PHONY: destroy
destroy: ## Tear down, platform first. The database has prevent_destroy set.
	tofu -chdir=$(PLATFORM) destroy
	tofu -chdir=$(CLUSTER) destroy
