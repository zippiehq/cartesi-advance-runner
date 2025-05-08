arget "docker-metadata-action" {
  context = "."
}
target "docker-platforms" {
  platforms = [
    "linux/amd64",
    "linux/arm64"
  ]
}


target "default" {
  inherits = ["docker-metadata-action", "docker-platforms"]
}
