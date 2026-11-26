CARGO ?= cargo
INSTALL ?= install
PREFIX ?= /usr

BIN := t2-touchbar-ultra
UI_BIN := t2-touchbar-ultra-settings
GREETER := t2-touchbar-ultra-greeter
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)/release

BIN_DIR := $(DESTDIR)$(PREFIX)/bin
USER_UNIT_DIR := $(DESTDIR)$(PREFIX)/lib/systemd/user
SYSTEM_UNIT_DIR := $(DESTDIR)$(PREFIX)/lib/systemd/system
UDEV_DIR := $(DESTDIR)$(PREFIX)/lib/udev/rules.d
SYSUSERS_DIR := $(DESTDIR)$(PREFIX)/lib/sysusers.d
TMPFILES_DIR := $(DESTDIR)$(PREFIX)/lib/tmpfiles.d
APPS_DIR := $(DESTDIR)$(PREFIX)/share/applications
ICONS_APP_DIR := $(DESTDIR)$(PREFIX)/share/icons/hicolor/scalable/apps
SHARE_DIR := $(DESTDIR)$(PREFIX)/share/t2-touchbar-ultra
CONFIG_DIR := $(DESTDIR)/etc/t2-touchbar-ultra
LICENSE_DIR := $(DESTDIR)$(PREFIX)/share/licenses/t2-touchbar-ultra

.PHONY: all build test check-build install uninstall clean

all: build

build:
	$(CARGO) build --release --workspace --locked

test:
	$(CARGO) test --workspace --locked

check-build:
	@test -x "$(TARGET_DIR)/$(BIN)" || { echo "error: $(TARGET_DIR)/$(BIN) is missing; run make build first" >&2; exit 1; }
	@test -x "$(TARGET_DIR)/$(UI_BIN)" || { echo "error: $(TARGET_DIR)/$(UI_BIN) is missing; run make build first" >&2; exit 1; }

install: check-build
	$(INSTALL) -d -m 0755 $(BIN_DIR) $(USER_UNIT_DIR) $(SYSTEM_UNIT_DIR) $(UDEV_DIR) \
		$(SYSUSERS_DIR) $(TMPFILES_DIR) $(APPS_DIR) $(ICONS_APP_DIR) \
		$(SHARE_DIR)/icons $(SHARE_DIR)/dict $(CONFIG_DIR) $(LICENSE_DIR)
	$(INSTALL) -m 0755 $(TARGET_DIR)/$(BIN) $(BIN_DIR)/$(BIN)
	$(INSTALL) -m 0755 $(TARGET_DIR)/$(UI_BIN) $(BIN_DIR)/$(UI_BIN)
	$(INSTALL) -m 0755 integration/greetd/$(GREETER) $(BIN_DIR)/$(GREETER)
	$(INSTALL) -m 0644 integration/systemd/user/t2-touchbar-ultra.service $(USER_UNIT_DIR)/t2-touchbar-ultra.service
	$(INSTALL) -m 0644 integration/systemd/system/t2-touchbar-ultra-attach.service $(SYSTEM_UNIT_DIR)/t2-touchbar-ultra-attach.service
	$(INSTALL) -m 0644 integration/udev/99-zz-t2-touchbar-ultra.rules $(UDEV_DIR)/99-zz-t2-touchbar-ultra.rules
	$(INSTALL) -m 0644 integration/sysusers.d/t2-touchbar-ultra.conf $(SYSUSERS_DIR)/t2-touchbar-ultra.conf
	$(INSTALL) -m 0644 integration/tmpfiles.d/t2-touchbar-ultra.conf $(TMPFILES_DIR)/t2-touchbar-ultra.conf
	$(INSTALL) -m 0644 integration/desktop/org.t2touchbarultra.gtk.desktop $(APPS_DIR)/org.t2touchbarultra.gtk.desktop
	$(INSTALL) -m 0644 assets/icons/hicolor/scalable/apps/org.t2touchbarultra.gtk.svg $(ICONS_APP_DIR)/org.t2touchbarultra.gtk.svg
	$(INSTALL) -m 0644 icons/*.svg $(SHARE_DIR)/icons/
	$(INSTALL) -m 0644 dict/* $(SHARE_DIR)/dict/
	$(INSTALL) -m 0644 LICENSE $(LICENSE_DIR)/LICENSE
	$(INSTALL) -m 0644 THIRD-PARTY-NOTICES.md $(LICENSE_DIR)/THIRD-PARTY-NOTICES.md
	@test -e $(CONFIG_DIR)/touchbar.toml || $(INSTALL) -m 0644 config/touchbar.toml $(CONFIG_DIR)/touchbar.toml
	@test -e $(CONFIG_DIR)/theme.toml || $(INSTALL) -m 0644 config/theme.toml $(CONFIG_DIR)/theme.toml

uninstall:
	rm -f $(BIN_DIR)/$(BIN) $(BIN_DIR)/$(UI_BIN) $(BIN_DIR)/$(GREETER)
	rm -f $(USER_UNIT_DIR)/t2-touchbar-ultra.service
	rm -f $(SYSTEM_UNIT_DIR)/t2-touchbar-ultra-attach.service
	rm -f $(UDEV_DIR)/99-zz-t2-touchbar-ultra.rules
	rm -f $(SYSUSERS_DIR)/t2-touchbar-ultra.conf
	rm -f $(TMPFILES_DIR)/t2-touchbar-ultra.conf
	rm -f $(APPS_DIR)/org.t2touchbarultra.gtk.desktop
	rm -f $(ICONS_APP_DIR)/org.t2touchbarultra.gtk.svg
	rm -rf $(SHARE_DIR) $(LICENSE_DIR)

clean:
	$(CARGO) clean
