# main.tf
# Deploy the actual Kubernetes cluster
resource "digitalocean_kubernetes_cluster" "ursa_cluster" {

  for_each = toset(var.regions)

  region = each.value
  name   = "ursa-${each.value}"

  version = "1.23.9-do.0"

  #   tags = ["my-tag"]

  # This default node pool is mandatory
  node_pool {
    name       = "ursa-main"
    size       = var.k8s_droplet_size
    auto_scale = true
    min_nodes  = var.k8s_min_node_count
    max_nodes  = var.k8s_max_node_count
    tags       = ["ursa-main"]
  }

}


# Another node pool in case we need node affinity etc
# resource "digitalocean_kubernetes_node_pool" "app_node_pool" {
#   cluster_id = digitalocean_kubernetes_cluster.kubernetes_cluster.id

#   name = "app-pool"
#   size = "s-2vcpu-4gb" # bigger instances
#   tags = ["applications"]

#   # you can setup autoscaling
#   auto_scale = true
#   min_nodes  = 2
#   max_nodes  = 5
#   labels = {
#     service  = "apps"
#     priority = "high"
#   }
# }


# Kubernetes Provider

resource "digitalocean_project_resources" "project_resources" {
  project = digitalocean_project.ursa.id

  for_each = toset(var.regions)

  resources = [
    digitalocean_kubernetes_cluster.ursa_cluster[each.value].urn
  ]
}

resource "kubernetes_namespace" "ursa" {
  metadata {
    name = "ursa"
  }
}

module "k8s_apps_ams3" {
  source = "./apps"

  k8s_host  = digitalocean_kubernetes_cluster.ursa_cluster.ams3.endpoint
  k8s_token = digitalocean_kubernetes_cluster.ursa_cluster.ams3.kube_config[0].token
  k8s_cluster_ca_certificate = base64decode(
    digitalocean_kubernetes_cluster.ursa_cluster.ams3.kube_config[0].cluster_ca_certificate
  )
}
