variable "aws_region" {
  description = "AWS region for the workload resources."
  type        = string
  default     = "us-east-2"
}

variable "domain_name" {
  description = "Apex domain to serve. Delegate it to the returned Route 53 name servers when Terraform creates the hosted zone."
  type        = string

  validation {
    condition     = can(regex("^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+$", var.domain_name))
    error_message = "domain_name must be a lowercase fully qualified domain name without a trailing dot."
  }
}

variable "hosted_zone_id" {
  description = "Existing public Route 53 hosted zone ID. Leave null to create a hosted zone."
  type        = string
  default     = null
  nullable    = true
}

variable "project_name" {
  description = "Lowercase name used in AWS resource names."
  type        = string
  default     = "swarm"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,20}$", var.project_name))
    error_message = "project_name must be 2-21 lowercase letters, numbers, or hyphens and start with a letter."
  }
}

variable "environment" {
  description = "Deployment environment name."
  type        = string
  default     = "production"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{1,14}$", var.environment))
    error_message = "environment must be 2-15 lowercase letters, numbers, or hyphens and start with a letter."
  }
}

variable "instance_type" {
  description = "EC2 instance size. t3.micro is free-tier eligible for qualifying accounts."
  type        = string
  default     = "t3.micro"
}

variable "root_volume_size" {
  description = "EC2 gp3 root volume size in GiB."
  type        = number
  default     = 8

  validation {
    condition     = var.root_volume_size >= 8
    error_message = "root_volume_size must be at least 8 GiB."
  }
}

variable "data_volume_size" {
  description = "Persistent gp3 volume size in GiB for SQLite and Caddy state."
  type        = number
  default     = 8

  validation {
    condition     = var.data_volume_size >= 1
    error_message = "data_volume_size must be at least 1 GiB."
  }
}

variable "stun_subdomain" {
  description = "DNS label for the STUN/rendezvous service."
  type        = string
  default     = "stun"

  validation {
    condition     = can(regex("^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$", var.stun_subdomain))
    error_message = "stun_subdomain must be a valid lowercase DNS label."
  }
}

variable "web_app_subdomain" {
  description = "DNS label for the prompt-data web app."
  type        = string
  default     = "app"

  validation {
    condition     = can(regex("^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$", var.web_app_subdomain))
    error_message = "web_app_subdomain must be a valid lowercase DNS label."
  }
}

variable "relay_port" {
  description = "Public TCP and UDP port for the fallback relay, hosted on the Oracle Cloud instance (see relay-oracle.tf)."
  type        = number
  default     = 8443

  validation {
    condition     = var.relay_port >= 1024 && var.relay_port <= 65535 && var.relay_port != 3478
    error_message = "relay_port must be between 1024 and 65535 and cannot be the reserved reflector port 3478."
  }
}

variable "allowed_relay_cidrs" {
  description = "IPv4 networks allowed to reach the relay port on the Oracle-hosted relay instance. Keep 0.0.0.0/0 for internet clients."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "relay_subdomain" {
  description = "DNS label for the fallback relay. It resolves to the Oracle instance's IP, separate from the AWS app host."
  type        = string
  default     = "relay"

  validation {
    condition     = can(regex("^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$", var.relay_subdomain))
    error_message = "relay_subdomain must be a valid lowercase DNS label."
  }
}

variable "ecr_image_tag" {
  description = "Image tag deployed for each AWS application repository (stun-server, prompt-web)."
  type        = string
  default     = "latest"
}

variable "tags" {
  description = "Additional tags applied to AWS resources."
  type        = map(string)
  default     = {}
}

# --- Oracle Cloud: fallback relay (Always Free tier) ---
#
# The relay is the only workload hosted outside AWS. It runs on an Oracle
# Cloud Infrastructure Always Free compute instance to avoid a second AWS
# EC2/EBS bill, and publishes its container image to OCI Container Registry
# (OCIR) instead of ECR. See relay-oracle.tf and the README for setup.

variable "oci_tenancy_ocid" {
  description = "OCID of the Oracle Cloud tenancy that hosts the relay instance."
  type        = string
}

variable "oci_user_ocid" {
  description = "OCID of the Oracle Cloud user Terraform authenticates as."
  type        = string
}

variable "oci_username" {
  description = "OCI username (e.g. the account email address) paired with oci_user_ocid, used for OCIR docker login."
  type        = string
}

variable "oci_fingerprint" {
  description = "Fingerprint of the OCI API signing key configured for oci_user_ocid."
  type        = string
}

variable "oci_private_key_path" {
  description = "Path to the private key half of the OCI API signing key."
  type        = string
  default     = "~/.oci/oci_api_key.pem"
}

variable "oci_region" {
  description = "OCI region for the relay instance. Always Free compute shapes are only available in your tenancy's home region."
  type        = string
}

variable "oci_compartment_ocid" {
  description = "Compartment for the relay resources. Defaults to the tenancy's root compartment."
  type        = string
  default     = null
  nullable    = true
}

variable "oci_registry_region_key" {
  description = "Short region key OCI uses for Container Registry hostnames (e.g. \"iad\" for us-ashburn-1). Required when oci_region isn't in the built-in lookup table in relay-oracle.tf."
  type        = string
  default     = null
  nullable    = true
}

variable "relay_instance_shape" {
  description = "OCI Always Free compute shape for the relay instance."
  type        = string
  default     = "VM.Standard.E2.1.Micro"
}

variable "relay_image_tag" {
  description = "Image tag deployed from the OCI Container Registry relay repository."
  type        = string
  default     = "latest"
}

variable "relay_ssh_public_key" {
  description = "SSH public key installed on the relay instance. Leave null to skip SSH key injection (the instance stays reachable only through the OCI console's serial console)."
  type        = string
  default     = null
  nullable    = true
}

variable "relay_ssh_allowed_cidrs" {
  description = "IPv4 networks allowed to SSH into the relay instance. Leave empty (default) to disable SSH ingress entirely."
  type        = list(string)
  default     = []
}
