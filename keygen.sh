nix shell nixpkgs#openssl --command openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout mitm-proxy-ca.key \
  -out mitm-proxy-ca.pem \
  -days 3650 \
  -subj "/CN=agentsandbox-mitm-ca" \
  -addext "basicConstraints=critical,CA:TRUE" \
  -addext "keyUsage=critical,digitalSignature,keyCertSign,cRLSign" \
  -addext "extendedKeyUsage=serverAuth" \
  -addext "subjectAltName=DNS:auth.docker.io,DNS:registry-1.docker.io"