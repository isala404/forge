import type {
  EvaluationContext,
  JsonValue,
  Provider,
  ResolutionDetails,
} from '@openfeature/server-sdk';
import type { EventHook } from '@openfeature/open-telemetry-hooks';

export interface ForgeFlagClient {
  flagDetails(
    key: string,
    defaultJson: string,
    targetingKey?: string,
  ): Promise<{
    valueJson: string;
    valueType: string;
    variant?: string | null;
    reason: string;
    errorCode?: string | null;
  }>;
}

export declare class ForgeProvider implements Provider {
  readonly runsOn: 'server';
  readonly metadata: Readonly<{ name: 'forge' }>;
  constructor(forge: ForgeFlagClient);
  resolveBooleanEvaluation(flagKey: string, defaultValue: boolean, context?: EvaluationContext): Promise<ResolutionDetails<boolean>>;
  resolveStringEvaluation(flagKey: string, defaultValue: string, context?: EvaluationContext): Promise<ResolutionDetails<string>>;
  resolveNumberEvaluation(flagKey: string, defaultValue: number, context?: EvaluationContext): Promise<ResolutionDetails<number>>;
  resolveObjectEvaluation<T extends JsonValue>(flagKey: string, defaultValue: T, context?: EvaluationContext): Promise<ResolutionDetails<T>>;
}

/** Construct the official OTel `feature_flag.evaluation` event hook without registering it globally. */
export declare function telemetryHook(): EventHook;
