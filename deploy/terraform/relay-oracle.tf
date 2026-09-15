# Fallback relay: the one workload hosted on Oracle Cloud instead of AWS.
#
# Oracle's Always Free tier includes small AMD compute shapes
# (VM.Standard.E2.1.Micro), a reserved public IP, and enough block/object
# storage for a single stateless relay container, at no cost. Everything
# else in this deployment (STUN, the static site, the prompt web app, and
# all DNS) stays on AWS; this file only reaches into Oracle for the relay's
# compute and container registry.

locals {
  oci_compartment_id = coalesce(var.oci_compartment_ocid, var.oci_tenancy_ocid)
  relay_domain       = "${var.relay_subdomain}.${var.domain_name}"

  # OCI Container Registry hostnames use a short per-region key rather than
  # the region name itself. This covers the regions commonly used as an
  # Always Free home region; set oci_registry_region_key to override or add
  # to it for an uncovered region.
  oci_registry_region_keys = {
    "us-ashburn-1"   = "iad"
    "us-phoenix-1"   = "phx"
    "uk-london-1"    = "lhr"
    "eu-frankfurt-1" = "fra"
    "eu-amsterdam-1" = "ams"
    "eu-zurich-1"    = "zrh"
    "ap-mumbai-1"    = "bom"
    "ap-singapore-1" = "sin"
    "ap-sydney-1"    = "syd"
    "ap-tokyo-1"     = "nrt"
    "ap-osaka-1"     = "kix"
    "ca-toronto-1"   = "yyz"
    "sa-saopaulo-1"  = "gru"
  }
  oci_registry_region_key   = coalesce(var.oci_registry_region_key, lookup(local.oci_registry_region_keys, var.oci_region, null))
  relay_registry_host       = "${local.oci_registry_region_key}.ocir.io"
  relay_registry_repository = "${data.oci_objectstorage_namespace.relay.namespace}/${local.name_prefix}/relay-server"
}

data "oci_objectstorage_namespace" "relay" {
  compartment_id = local.oci_compartment_id
}

data "oci_identity_availability_domains" "relay" {
  compartment_id = var.oci_tenancy_ocid
}

data "oci_core_images" "relay" {
  compartment_id           = local.oci_compartment_id
  operating_system         = "Oracle Linux"
  operating_system_version = "9"
  shape                    = var.relay_instance_shape
  sort_by                  = "TIMECREATED"
  sort_order               = "DESC"
}

resource "oci_artifacts_container_repository" "relay" {
  compartment_id = local.oci_compartment_id
  display_name   = "${local.name_prefix}/relay-server"
  is_public      = false
  freeform_tags  = local.common_tags
}

resource "oci_identity_auth_token" "relay_registry" {
  user_id     = var.oci_user_ocid
  description = "${local.name_prefix} relay OCIR pull/push token"
}

resource "oci_core_vcn" "relay" {
  compartment_id = local.oci_compartment_id
  cidr_blocks    = ["10.77.0.0/16"]
  display_name   = "${local.name_prefix}-relay"
  dns_label      = replace("${var.project_name}relay", "-", "")
  freeform_tags  = local.common_tags
}

resource "oci_core_internet_gateway" "relay" {
  compartment_id = local.oci_compartment_id
  vcn_id         = oci_core_vcn.relay.id
  display_name   = "${local.name_prefix}-relay"
  enabled        = true
}

resource "oci_core_route_table" "relay" {
  compartment_id = local.oci_compartment_id
  vcn_id         = oci_core_vcn.relay.id
  display_name   = "${local.name_prefix}-relay"

  route_rules {
    destination       = "0.0.0.0/0"
    network_entity_id = oci_core_internet_gateway.relay.id
  }
}

resource "oci_core_security_list" "relay" {
  compartment_id = local.oci_compartment_id
  vcn_id         = oci_core_vcn.relay.id
  display_name   = "${local.name_prefix}-relay"

  egress_security_rules {
    destination = "0.0.0.0/0"
    protocol    = "all"
  }

  # Required by OCI so path MTU discovery works on the public subnet.
  ingress_security_rules {
    description = "Path MTU discovery"
    source      = "0.0.0.0/0"
    protocol    = "1"
    icmp_options {
      type = 3
      code = 4
    }
  }

  dynamic "ingress_security_rules" {
    for_each = var.allowed_relay_cidrs
    content {
      description = "Fallback relay TCP"
      source      = ingress_security_rules.value
      protocol    = "6"
      tcp_options {
        min = var.relay_port
        max = var.relay_port
      }
    }
  }

  dynamic "ingress_security_rules" {
    for_each = var.allowed_relay_cidrs
    content {
      description = "Fallback relay UDP"
      source      = ingress_security_rules.value
      protocol    = "17"
      udp_options {
        min = var.relay_port
        max = var.relay_port
      }
    }
  }

  dynamic "ingress_security_rules" {
    for_each = var.relay_ssh_allowed_cidrs
    content {
      description = "SSH administration"
      source      = ingress_security_rules.value
      protocol    = "6"
      tcp_options {
        min = 22
        max = 22
      }
    }
  }
}

resource "oci_core_subnet" "relay" {
  compartment_id             = local.oci_compartment_id
  vcn_id                     = oci_core_vcn.relay.id
  cidr_block                 = "10.77.1.0/24"
  display_name               = "${local.name_prefix}-relay-public"
  route_table_id             = oci_core_route_table.relay.id
  security_list_ids          = [oci_core_security_list.relay.id]
  prohibit_public_ip_on_vnic = false
  dns_label                  = "public"
}

resource "oci_core_instance" "relay" {
  compartment_id      = local.oci_compartment_id
  availability_domain = data.oci_identity_availability_domains.relay.availability_domains[0].name
  shape               = var.relay_instance_shape
  display_name        = "${local.name_prefix}-relay"
  freeform_tags       = local.common_tags

  create_vnic_details {
    subnet_id        = oci_core_subnet.relay.id
    assign_public_ip = true
  }

  source_details {
    source_type = "image"
    source_id   = data.oci_core_images.relay.images[0].id
  }

  metadata = merge(
    var.relay_ssh_public_key != null ? { ssh_authorized_keys = var.relay_ssh_public_key } : {},
    {
      user_data = base64encode(templatefile("${path.module}/templates/relay-user-data.sh.tftpl", {
        registry_host       = local.relay_registry_host
        registry_repository = local.relay_registry_repository
        registry_username   = "${data.oci_objectstorage_namespace.relay.namespace}/${var.oci_username}"
        registry_token      = oci_identity_auth_token.relay_registry.token
        image_tag           = var.relay_image_tag
        relay_port          = var.relay_port
        relay_domain        = local.relay_domain
      }))
    }
  )
}

# Reserved public IP so the relay's DNS record survives instance stop/start.
data "oci_core_vnic_attachments" "relay" {
  compartment_id = local.oci_compartment_id
  instance_id    = oci_core_instance.relay.id
}

data "oci_core_vnic" "relay" {
  vnic_id = data.oci_core_vnic_attachments.relay.vnic_attachments[0].vnic_id
}

data "oci_core_private_ips" "relay" {
  ip_address = data.oci_core_vnic.relay.private_ip_address
  subnet_id  = oci_core_subnet.relay.id
}

resource "oci_core_public_ip" "relay" {
  compartment_id = local.oci_compartment_id
  lifetime       = "RESERVED"
  display_name   = "${local.name_prefix}-relay"
  private_ip_id  = data.oci_core_private_ips.relay.private_ips[0].id
}

resource "aws_route53_record" "relay" {
  zone_id = local.hosted_zone_id
  name    = local.relay_domain
  type    = "A"
  ttl     = 300
  records = [oci_core_public_ip.relay.ip_address]
}
