FROM registry.access.redhat.com/ubi9-minimal
ARG BINARY=target/release/controller

LABEL maintainer="Tobias Florek <tob@butter.sh>"

EXPOSE 8080/tcp 9880/tcp

COPY $BINARY /controller

CMD ["/controller"]
USER 1000
