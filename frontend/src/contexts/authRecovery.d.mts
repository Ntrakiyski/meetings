export interface RecoveredAuthSession {
  userId: string
  issuer: string
  organizationId: string
  workspaceId: string
  expiresAt: number
}

export function recoverAuthenticatedSession(
  invoke: (command: string) => Promise<unknown>,
): Promise<RecoveredAuthSession | null>

export function createAuthSessionRecovery(
  invoke: (command: string) => Promise<unknown>,
): { refresh(): Promise<RecoveredAuthSession | null> }

export function retryPendingFinalSync(
  invoke: (command: string) => Promise<unknown>,
): Promise<unknown>
