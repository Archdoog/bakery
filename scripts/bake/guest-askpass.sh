#!/bin/sh
# SSH_ASKPASS helper for the bake. Prints the guest password from BAKE_GUEST_PASS.
printf '%s\n' "$BAKE_GUEST_PASS"
