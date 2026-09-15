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
  description = "Public TCP and UDP port reserved for the future relay container."
  type        = number
  default     = 8443

  validation {
    condition     = var.relay_port >= 1024 && var.relay_port <= 65535 && var.relay_port != 3478
    error_message = "relay_port must be between 1024 and 65535 and cannot be the reserved reflector port 3478."
  }
}

variable "allowed_relay_cidrs" {
  description = "IPv4 networks allowed to reach the relay port. Keep 0.0.0.0/0 for internet clients."
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

variable "ecr_image_tag" {
  description = "Image tag deployed for each application repository."
  type        = string
  default     = "latest"
}

variable "tags" {
  description = "Additional tags applied to AWS resources."
  type        = map(string)
  default     = {}
}
