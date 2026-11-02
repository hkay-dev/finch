#!/usr/bin/env bash
# Sourced by the scripts that sign a bundle. Sets SIGN_IDENTITY to the first of:
#   $FINCH_SIGN_IDENTITY
#   the first line of ${XDG_CONFIG_HOME:-$HOME/.config}/finch/sign-identity, if that file exists
#   `-` (an ad-hoc signature), with one line on stderr saying so.
# A stable certificate keeps macOS permission prompts from coming back after every rebuild.
finch_sign_identity() {
    local file="${XDG_CONFIG_HOME:-$HOME/.config}/finch/sign-identity" first=""
    if [[ -n ${FINCH_SIGN_IDENTITY:-} ]]; then
        SIGN_IDENTITY=$FINCH_SIGN_IDENTITY
    elif [[ -f $file ]] && IFS= read -r first <"$file" || [[ -n $first ]]; then
        SIGN_IDENTITY=$first
    fi
    if [[ -z ${SIGN_IDENTITY:-} ]]; then
        SIGN_IDENTITY=-
        echo "sign: no signing identity set, signing ad hoc (set FINCH_SIGN_IDENTITY, or put a certificate name in $file)" >&2
    fi
}
finch_sign_identity
