'use strict';

const { ErrorCode, StandardResolutionReasons } = require('@openfeature/server-sdk');

const REASONS = Object.freeze({
  static: StandardResolutionReasons.STATIC,
  percent_in: StandardResolutionReasons.SPLIT,
  percent_out: StandardResolutionReasons.SPLIT,
  targeting_match: StandardResolutionReasons.TARGETING_MATCH,
  targeting_miss: StandardResolutionReasons.TARGETING_MATCH,
  default_error: StandardResolutionReasons.ERROR,
  default_closed: StandardResolutionReasons.ERROR,
});

class ForgeProvider {
  constructor(forge) {
    if (!forge || typeof forge.flagDetails !== 'function') {
      throw new TypeError('forge must be an initialized ForgeClient');
    }
    this.forge = forge;
    this.runsOn = 'server';
    this.metadata = Object.freeze({ name: 'forge' });
    Object.freeze(this);
  }

  async resolveBooleanEvaluation(flagKey, defaultValue, context = {}) {
    return this.#resolve(flagKey, defaultValue, context, value => typeof value === 'boolean');
  }

  async resolveStringEvaluation(flagKey, defaultValue, context = {}) {
    return this.#resolve(flagKey, defaultValue, context, value => typeof value === 'string');
  }

  async resolveNumberEvaluation(flagKey, defaultValue, context = {}) {
    return this.#resolve(flagKey, defaultValue, context, value => typeof value === 'number' && Number.isFinite(value));
  }

  async resolveObjectEvaluation(flagKey, defaultValue, context = {}) {
    return this.#resolve(flagKey, defaultValue, context, value => value !== null && typeof value === 'object');
  }

  async #resolve(flagKey, defaultValue, context, accepts) {
    let details;
    try {
      details = await this.forge.flagDetails(flagKey, JSON.stringify(defaultValue), targetingKey(context));
    } catch {
      return failure(defaultValue, ErrorCode.GENERAL, 'Forge evaluation failed');
    }
    let value;
    try {
      value = JSON.parse(details.valueJson);
    } catch {
      return failure(defaultValue, ErrorCode.PARSE_ERROR, 'Forge returned invalid JSON');
    }
    if (!accepts(value)) {
      return failure(defaultValue, ErrorCode.TYPE_MISMATCH, 'flag value has the wrong type');
    }
    if (details.errorCode) {
      return failure(value, ErrorCode.GENERAL, 'Forge evaluation failed', details.variant);
    }
    if (details.reason === 'default_missing') {
      return failure(value, ErrorCode.FLAG_NOT_FOUND, 'flag was not found');
    }
    return {
      value,
      reason: REASONS[details.reason] || StandardResolutionReasons.DEFAULT,
      ...(details.variant ? { variant: details.variant } : {}),
    };
  }
}

function targetingKey(context) {
  return typeof context.targetingKey === 'string' && context.targetingKey ? context.targetingKey : undefined;
}

function failure(value, errorCode, errorMessage, variant) {
  return {
    value,
    reason: StandardResolutionReasons.ERROR,
    errorCode,
    errorMessage,
    ...(variant ? { variant } : {}),
  };
}

function telemetryHook() {
  const { EventHook } = require('@openfeature/open-telemetry-hooks');
  return new EventHook();
}

module.exports = { ForgeProvider, telemetryHook };
