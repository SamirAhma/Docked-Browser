# Docked Browser image: Google Chrome + fonts + EGL/Pulse libs.
# Built as: local-docked-browser  (see bin/docked-browser build / AGENTS.md)
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

# Install base dependencies, X11/Wayland libraries, PulseAudio client, and Google Chrome
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    gnupg \
    unzip \
    hicolor-icon-theme \
    libcanberra-gtk3-0 \
    libgl1 \
    libgl1-mesa-dri \
    libegl1 \
    libgles2 \
    libgbm1 \
    libpulse0 \
    libv4l-0 \
    fontconfig \
    fonts-liberation \
    fonts-noto-core \
    fonts-noto-color-emoji \
    fonts-noto-ui-core \
    tini \
    && curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google-chrome.gpg \
    && echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google-chrome.gpg] http://dl.google.com/linux/chrome/deb/ stable main" > /etc/apt/sources.list.d/google-chrome.list \
    && apt-get update && apt-get install -y --no-install-recommends \
    google-chrome-stable \
    && rm -rf /var/lib/apt/lists/*

# Mac-like UI fonts: Inter (SF stand-in) + JetBrains Mono (Menlo stand-in)
RUN mkdir -p /usr/local/share/fonts/inter /usr/local/share/fonts/jetbrains-mono \
    && curl -fsSL -o /tmp/Inter.zip https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip \
    && unzip -q /tmp/Inter.zip -d /tmp/Inter \
    && find /tmp/Inter -type f \( -name 'Inter*.otf' -o -name 'Inter*.ttf' \) -exec cp {} /usr/local/share/fonts/inter/ \; \
    && curl -fsSL -o /tmp/JBM.zip https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip \
    && unzip -q /tmp/JBM.zip -d /tmp/JBM \
    && find /tmp/JBM -type f -name '*.ttf' -exec cp {} /usr/local/share/fonts/jetbrains-mono/ \; \
    && rm -rf /tmp/Inter /tmp/Inter.zip /tmp/JBM /tmp/JBM.zip \
    && fc-cache -f

COPY fontconfig/99-docked-browser-fonts.conf /etc/fonts/conf.d/99-docked-browser-fonts.conf
RUN fc-cache -f

# Run as non-root user for security matching host UID
RUN useradd -ms /bin/bash chromeuser && \
    usermod -aG audio,video chromeuser

USER chromeuser
ENV HOME=/home/chromeuser

# Use tini as init process to prevent zombie processes
ENTRYPOINT ["/usr/bin/tini", "--", "google-chrome"]
CMD ["--no-first-run", "--no-default-browser-check"]
