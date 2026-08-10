// CRMSwift Metadata Configuration
// Everything configurable - no hardcoded values

export interface MetadataField {
  id: string;
  key: string;
  label: string;
  type: 'text' | 'number' | 'date' | 'select' | 'multiselect' | 'boolean' | 'url' | 'email' | 'phone';
  options?: string[];
  defaultValue?: any;
  required?: boolean;
  placeholder?: string;
  helpText?: string;
  order: number;
}

export interface PipelineStage {
  id: string;
  name: string;
  color: string;
  order: number;
  probability: number; // Close probability %
  metadataFields: string[]; // Which fields show in this stage
}

export interface Pipeline {
  id: string;
  name: string;
  description?: string;
  stages: PipelineStage[];
  isDefault: boolean;
}

export interface DealType {
  id: string;
  name: string;
  icon: string;
  color: string;
  defaultValue: number;
}

export interface ActivityType {
  id: string;
  name: string;
  icon: string;
  color: string;
  requiresOutcome: boolean;
  outcomes?: string[];
}

export interface CRMMetadata {
  // Core entities
  dealFields: MetadataField[];
  contactFields: MetadataField[];
  companyFields: MetadataField[];
  activityFields: MetadataField[];
  
  // Pipeline config
  pipelines: Pipeline[];
  
  // Categorization
  dealTypes: DealType[];
  activityTypes: ActivityType[];
  
  // Tags & labels
  tags: string[];
  sources: string[];
  
  // Branding
  branding: {
    appName: string;
    logoUrl?: string;
    faviconUrl?: string;
    primaryColor: string;
    accentColor: string;
    customCss?: string;
  };
  
  // Feature toggles (override plan features)
  featureOverrides: Record<string, boolean>;
  
  // Integration settings
  integrations: {
    adaswift?: { enabled: boolean; webhookUrl?: string };
    missedcall?: { enabled: boolean; webhookUrl?: string };
    workflowswift?: { enabled: boolean; webhookUrl?: string };
    mintbird?: { enabled: boolean; partnerId?: string };
    n8n?: { enabled: boolean; webhookUrl?: string };
  };
}

// Default metadata - loaded from DB but these are fallbacks
export const DEFAULT_METADATA: CRMMetadata = {
  dealFields: [
    { id: 'title', key: 'title', label: 'Deal Title', type: 'text', required: true, order: 1 },
    { id: 'value', key: 'value', label: 'Deal Value', type: 'number', required: true, placeholder: '0', order: 2 },
    { id: 'company', key: 'company', label: 'Company', type: 'text', required: true, order: 3 },
    { id: 'contact', key: 'contact', label: 'Primary Contact', type: 'text', order: 4 },
    { id: 'email', key: 'email', label: 'Email', type: 'email', order: 5 },
    { id: 'phone', key: 'phone', label: 'Phone', type: 'phone', order: 6 },
    { id: 'source', key: 'source', label: 'Source', type: 'select', options: ['Website', 'Referral', 'Cold Call', 'ADASwift', 'MissedCall', 'WorkflowSwift'], order: 7 },
    { id: 'expectedClose', key: 'expected_close_date', label: 'Expected Close', type: 'date', order: 8 },
    { id: 'notes', key: 'notes', label: 'Notes', type: 'text', placeholder: 'Add notes...', order: 9 },
  ],
  
  contactFields: [
    { id: 'firstName', key: 'first_name', label: 'First Name', type: 'text', required: true, order: 1 },
    { id: 'lastName', key: 'last_name', label: 'Last Name', type: 'text', required: true, order: 2 },
    { id: 'email', key: 'email', label: 'Email', type: 'email', required: true, order: 3 },
    { id: 'phone', key: 'phone', label: 'Phone', type: 'phone', order: 4 },
    { id: 'company', key: 'company', label: 'Company', type: 'text', order: 5 },
    { id: 'title', key: 'job_title', label: 'Job Title', type: 'text', order: 6 },
    { id: 'source', key: 'source', label: 'Source', type: 'select', options: ['Website', 'Referral', 'Cold Call', 'ADASwift', 'MissedCall', 'WorkflowSwift'], order: 7 },
    { id: 'tags', key: 'tags', label: 'Tags', type: 'multiselect', order: 8 },
  ],
  
  companyFields: [
    { id: 'name', key: 'name', label: 'Company Name', type: 'text', required: true, order: 1 },
    { id: 'website', key: 'website', label: 'Website', type: 'url', order: 2 },
    { id: 'industry', key: 'industry', label: 'Industry', type: 'select', options: ['Technology', 'Healthcare', 'Finance', 'Retail', 'Manufacturing', 'Other'], order: 3 },
    { id: 'size', key: 'company_size', label: 'Company Size', type: 'select', options: ['1-10', '11-50', '51-200', '201-500', '500+'], order: 4 },
    { id: 'address', key: 'address', label: 'Address', type: 'text', order: 5 },
  ],
  
  activityFields: [
    { id: 'type', key: 'type', label: 'Activity Type', type: 'select', required: true, options: ['Call', 'Email', 'Meeting', 'Note', 'Task'], order: 1 },
    { id: 'subject', key: 'subject', label: 'Subject', type: 'text', required: true, order: 2 },
    { id: 'notes', key: 'notes', label: 'Notes', type: 'text', order: 3 },
    { id: 'outcome', key: 'outcome', label: 'Outcome', type: 'select', options: ['Completed', 'No Answer', 'Left Voicemail', 'Scheduled Follow-up', 'Not Interested'], order: 4 },
    { id: 'followUp', key: 'follow_up_date', label: 'Follow-up Date', type: 'date', order: 5 },
  ],
  
  pipelines: [
    {
      id: 'default',
      name: 'Sales Pipeline',
      isDefault: true,
      stages: [
        { id: 'prospecting', name: 'Prospecting', color: '#94a3b8', order: 1, probability: 10, metadataFields: ['title', 'value', 'company', 'contact'] },
        { id: 'qualified', name: 'Qualified', color: '#60a5fa', order: 2, probability: 25, metadataFields: ['title', 'value', 'company', 'contact', 'email', 'expectedClose'] },
        { id: 'proposal', name: 'Proposal', color: '#fbbf24', order: 3, probability: 50, metadataFields: ['title', 'value', 'company', 'contact', 'email', 'phone', 'expectedClose'] },
        { id: 'negotiation', name: 'Negotiation', color: '#f97316', order: 4, probability: 75, metadataFields: ['title', 'value', 'company', 'contact', 'email', 'phone', 'expectedClose', 'notes'] },
        { id: 'closed-won', name: 'Closed Won', color: '#22c55e', order: 5, probability: 100, metadataFields: ['title', 'value', 'company'] },
        { id: 'closed-lost', name: 'Closed Lost', color: '#ef4444', order: 6, probability: 0, metadataFields: ['title', 'value', 'company', 'notes'] },
      ],
    },
  ],
  
  dealTypes: [
    { id: 'new', name: 'New Business', icon: 'sparkles', color: '#22c55e', defaultValue: 0 },
    { id: 'existing', name: 'Existing Customer', icon: 'refresh', color: '#3b82f6', defaultValue: 0 },
    { id: 'renewal', name: 'Renewal', icon: 'repeat', color: '#f59e0b', defaultValue: 0 },
    { id: 'expansion', name: 'Expansion', icon: 'trending-up', color: '#8b5cf6', defaultValue: 0 },
  ],
  
  activityTypes: [
    { id: 'call', name: 'Call', icon: 'phone', color: '#22c55e', requiresOutcome: true, outcomes: ['Completed', 'No Answer', 'Left Voicemail', 'Callback Requested'] },
    { id: 'email', name: 'Email', icon: 'mail', color: '#3b82f6', requiresOutcome: true, outcomes: ['Sent', 'Opened', 'Replied', 'Bounced'] },
    { id: 'meeting', name: 'Meeting', icon: 'calendar', color: '#f59e0b', requiresOutcome: true, outcomes: ['Completed', 'No Show', 'Rescheduled'] },
    { id: 'note', name: 'Note', icon: 'file-text', color: '#6b7280', requiresOutcome: false },
    { id: 'task', name: 'Task', icon: 'check-square', color: '#8b5cf6', requiresOutcome: true, outcomes: ['Completed', 'In Progress', 'Blocked'] },
  ],
  
  tags: ['Hot Lead', 'Cold Lead', 'Follow-up Required', 'Decision Maker', 'Budget Approved', 'Technical Buyer', 'Champion'],
  sources: ['Website', 'Referral', 'Cold Call', 'ADASwift', 'MissedCall', 'WorkflowSwift', 'Social Media', 'Event', 'Partner'],
  
  branding: {
    appName: 'CRMSwift',
    primaryColor: '#18181b',
    accentColor: '#22c55e',
  },
  
  featureOverrides: {},
  
  integrations: {
    adaswift: { enabled: false },
    missedcall: { enabled: false },
    workflowswift: { enabled: false },
    mintbird: { enabled: false },
    n8n: { enabled: true },
  },
};

// Helper to get field by key
export function getField(fields: MetadataField[], key: string): MetadataField | undefined {
  return fields.find(f => f.key === key);
}

// Helper to get pipeline stage
export function getStage(pipeline: Pipeline, stageId: string): PipelineStage | undefined {
  return pipeline.stages.find(s => s.id === stageId);
}

// Helper to calculate weighted pipeline value
export function calculateWeightedValue(deals: Array<{ value: number; stageId: string }>, pipeline: Pipeline): number {
  return deals.reduce((total, deal) => {
    const stage = getStage(pipeline, deal.stageId);
    const probability = stage?.probability || 0;
    return total + (deal.value * (probability / 100));
  }, 0);
}
