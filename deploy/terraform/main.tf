data "aws_caller_identity" "current" {}

data "aws_ssm_parameter" "al2023_ami" {
  name = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
}

data "aws_availability_zones" "available" {
  state = "available"
}

locals {
  name_prefix = "${var.project_name}-${var.environment}"
  common_tags = merge({
    Application = "SWARM"
    Environment = var.environment
    ManagedBy   = "Terraform"
  }, var.tags)

  hosted_zone_id = var.hosted_zone_id != null ? var.hosted_zone_id : aws_route53_zone.this[0].zone_id
  stun_domain    = "${var.stun_subdomain}.${var.domain_name}"
  web_app_domain = "${var.web_app_subdomain}.${var.domain_name}"
  site_bucket    = "${local.name_prefix}-${data.aws_caller_identity.current.account_id}-site"
}

resource "aws_route53_zone" "this" {
  count = var.hosted_zone_id == null ? 1 : 0
  name  = var.domain_name
}

resource "aws_vpc" "this" {
  cidr_block           = "10.42.0.0/16"
  enable_dns_hostnames = true
  enable_dns_support   = true

  tags = { Name = local.name_prefix }
}

resource "aws_internet_gateway" "this" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = local.name_prefix }
}

resource "aws_subnet" "public" {
  vpc_id                  = aws_vpc.this.id
  cidr_block              = "10.42.1.0/24"
  availability_zone       = data.aws_availability_zones.available.names[0]
  map_public_ip_on_launch = true

  tags = { Name = "${local.name_prefix}-public" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.this.id

  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = aws_internet_gateway.this.id
  }

  tags = { Name = "${local.name_prefix}-public" }
}

resource "aws_route_table_association" "public" {
  subnet_id      = aws_subnet.public.id
  route_table_id = aws_route_table.public.id
}

resource "aws_security_group" "app_host" {
  name        = "${local.name_prefix}-app-host"
  description = "Public web, STUN reflector, and relay traffic"
  vpc_id      = aws_vpc.this.id

  ingress {
    description = "ACME challenge and HTTPS redirect"
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  ingress {
    description = "HTTPS and secure WebSockets"
    from_port   = 443
    to_port     = 443
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  ingress {
    description = "Primary UDP reflector"
    from_port   = 443
    to_port     = 443
    protocol    = "udp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  ingress {
    description = "Fallback UDP reflector"
    from_port   = 3478
    to_port     = 3478
    protocol    = "udp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }

  tags = { Name = "${local.name_prefix}-app-host" }
}

resource "aws_ecr_repository" "containers" {
  # The relay server runs on Oracle Cloud (see relay-oracle.tf) and publishes
  # to OCI Container Registry instead of ECR.
  for_each = toset(["stun-server", "prompt-web"])

  name                 = "${local.name_prefix}/${each.value}"
  image_tag_mutability = "MUTABLE"

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "AES256"
  }
}

resource "aws_ecr_lifecycle_policy" "containers" {
  for_each   = aws_ecr_repository.containers
  repository = each.value.name

  policy = jsonencode({
    rules = [{
      rulePriority = 1
      description  = "Keep the ten most recent images"
      selection = {
        tagStatus   = "any"
        countType   = "imageCountMoreThan"
        countNumber = 10
      }
      action = { type = "expire" }
    }]
  })
}

resource "aws_iam_role" "app_host" {
  name = "${local.name_prefix}-app-host"

  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "ec2.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.app_host.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_role_policy" "ecr_pull" {
  name = "ecr-pull"
  role = aws_iam_role.app_host.id

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect   = "Allow"
        Action   = ["ecr:GetAuthorizationToken"]
        Resource = "*"
      },
      {
        Effect = "Allow"
        Action = [
          "ecr:BatchCheckLayerAvailability",
          "ecr:BatchGetImage",
          "ecr:DescribeImages",
          "ecr:GetDownloadUrlForLayer"
        ]
        Resource = [for repository in aws_ecr_repository.containers : repository.arn]
      }
    ]
  })
}

resource "aws_iam_instance_profile" "app_host" {
  name = "${local.name_prefix}-app-host"
  role = aws_iam_role.app_host.name
}

resource "aws_instance" "app_host" {
  ami                    = data.aws_ssm_parameter.al2023_ami.value
  instance_type          = var.instance_type
  subnet_id              = aws_subnet.public.id
  vpc_security_group_ids = [aws_security_group.app_host.id]
  iam_instance_profile   = aws_iam_instance_profile.app_host.name

  user_data_replace_on_change = true
  user_data = templatefile("${path.module}/templates/user-data.sh.tftpl", {
    aws_region         = var.aws_region
    ecr_registry       = "${data.aws_caller_identity.current.account_id}.dkr.ecr.${var.aws_region}.amazonaws.com"
    image_tag          = var.ecr_image_tag
    stun_image         = aws_ecr_repository.containers["stun-server"].repository_url
    stun_repository    = aws_ecr_repository.containers["stun-server"].name
    web_app_image      = aws_ecr_repository.containers["prompt-web"].repository_url
    web_app_repository = aws_ecr_repository.containers["prompt-web"].name
    stun_domain        = local.stun_domain
    web_app_domain     = local.web_app_domain
    data_volume_id     = aws_ebs_volume.app_data.id
  })

  root_block_device {
    volume_type           = "gp3"
    volume_size           = var.root_volume_size
    encrypted             = true
    delete_on_termination = true
  }

  metadata_options {
    http_endpoint = "enabled"
    http_tokens   = "required"
  }

  tags = { Name = "${local.name_prefix}-app-host" }
}

resource "aws_ebs_volume" "app_data" {
  availability_zone = aws_subnet.public.availability_zone
  type              = "gp3"
  size              = var.data_volume_size
  encrypted         = true

  tags = { Name = "${local.name_prefix}-app-data" }
}

resource "aws_volume_attachment" "app_data" {
  device_name                    = "/dev/sdf"
  instance_id                    = aws_instance.app_host.id
  volume_id                      = aws_ebs_volume.app_data.id
  stop_instance_before_detaching = true
}

resource "aws_eip" "app_host" {
  domain   = "vpc"
  instance = aws_instance.app_host.id

  tags = { Name = "${local.name_prefix}-app-host" }
}

resource "aws_route53_record" "stun" {
  zone_id = local.hosted_zone_id
  name    = local.stun_domain
  type    = "A"
  ttl     = 300
  records = [aws_eip.app_host.public_ip]
}

resource "aws_route53_record" "web_app" {
  zone_id = local.hosted_zone_id
  name    = local.web_app_domain
  type    = "A"
  ttl     = 300
  records = [aws_eip.app_host.public_ip]
}
