export type CloudProvider = 'quark' | 'pikpak';
export type CloudAuthMethod = 'web' | 'password';

export interface CloudAccount { provider: CloudProvider; id: string; displayName: string; authMethod: CloudAuthMethod; expiresAt?: string; }
export interface CloudFile { id: string; parentId: string; name: string; kind: 'file' | 'folder'; size?: number; modifiedAt?: string; }
export interface CloudDirectory { id: string; items: CloudFile[]; }
export interface CloudAuthorization { provider: CloudProvider; windowLabel: string; instructions: string; }
export interface CloudCredentials { username: string; password: string; }
export interface CloudError { code: 'not_authorized' | 'credentials_expired' | 'web_auth_required' | 'authorization_pending' | 'credential_storage_error' | 'invalid_response' | 'network_error' | 'provider_error' | 'invalid_input' | 'provider_unsupported'; message: string; }
