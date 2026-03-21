FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y \
    xfce4 xfce4-terminal dbus-x11 \
    tigervnc-standalone-server novnc websockify \
    libwebkitgtk-6.0-dev libgtk-4-dev \
    build-essential pkg-config libssl-dev cmake \
    bubblewrap socat curl firefox mousepad thunar htop \
    fonts-inter locales ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && locale-gen en_US.UTF-8

# Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

COPY . /root/agent-os
WORKDIR /root/agent-os
RUN cargo build --release -p agentd -p agent-shell
RUN cp target/release/agentd /usr/local/bin/agentd && \
    cp target/release/agent-shell /usr/local/bin/agentos-gui
RUN mkdir -p /etc/agentos && cp config.yaml /etc/agentos/config.yaml

# VNC con XFCE + autostart agentd e GUI
RUN mkdir -p /root/.vnc && echo "agentos" | vncpasswd -f > /root/.vnc/passwd && chmod 600 /root/.vnc/passwd
RUN echo '#!/bin/sh\n\
export XDG_RUNTIME_DIR=/tmp/xdg-root\n\
mkdir -p /tmp/xdg-root\n\
dbus-launch startxfce4 &\n\
sleep 3\n\
/usr/local/bin/agentd &\n\
sleep 2\n\
GDK_BACKEND=x11 GSK_RENDERER=cairo /usr/local/bin/agentos-gui &' > /root/.vnc/xstartup && chmod +x /root/.vnc/xstartup

RUN echo '#!/bin/bash\n\
mkdir -p /tmp/xdg-root\n\
vncserver :1 -geometry 1920x1080 -depth 24 -localhost no\n\
sleep 3\n\
websockify --web=/usr/share/novnc/ 0.0.0.0:6080 localhost:5901' > /start.sh && chmod +x /start.sh

EXPOSE 6080
CMD ["/start.sh"]
