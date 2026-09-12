terraform { required_version = ">= 1.5" }
variable "region" { type = string; default = "us-east-1" }
module "network" {
  source = "./modules/network"
  cidr   = var.cidr
}
resource "aws_instance" "web" {
  ami           = data.aws_ami.ubuntu.id
  instance_type = "t3.micro"
  subnet_id     = module.network.subnet_id
  tags = { Name = "web-${var.region}" }
}
data "aws_ami" "ubuntu" { most_recent = true }
output "web_ip" { value = aws_instance.web.public_ip }
locals { cidr = "10.0.0.0/16" }
