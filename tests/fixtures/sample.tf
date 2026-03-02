variable "region" {
  description = "AWS region for resources"
  type        = string
  default     = "us-east-1"
}

variable "instance_count" {
  description = "Number of EC2 instances"
  type        = number
  default     = 2
}

resource "aws_instance" "web" {
  ami           = "ami-0c55b159cbfafe1f0"
  instance_type = "t3.micro"
  count         = var.instance_count

  tags = {
    Name = "web-server-${count.index}"
  }
}

resource "aws_security_group" "web_sg" {
  name        = "web-sg"
  description = "Security group for web servers"

  ingress {
    from_port   = 80
    to_port     = 80
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

output "instance_ids" {
  description = "IDs of created instances"
  value       = aws_instance.web[*].id
}

locals {
  common_tags = {
    Environment = "production"
    ManagedBy   = "terraform"
  }
}
