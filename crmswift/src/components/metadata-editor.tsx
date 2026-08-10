"use client";

import { useState } from "react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  Tabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@/components/ui/tabs";
import {
  MetadataField,
  Pipeline,
  PipelineStage,
  CRMMetadata,
  DEFAULT_METADATA,
} from "@/lib/metadata-config";
import { Plus, Trash, GripVertical, Settings, Palette, Plug } from "lucide-react";

// Main metadata manager
export function MetadataManager() {
  const [metadata, setMetadata] = useState<CRMMetadata>(DEFAULT_METADATA);
  const [activeTab, setActiveTab] = useState("fields");

  return (
    <div className="space-y-6">
      <div className="flex justify-between items-center">
        <div>
          <h2 className="text-2xl font-bold">CRM Configuration</h2>
          <p className="text-muted-foreground">
            Customize fields, pipelines, and branding
          </p>
        </div>
        <Button
          onClick={() => {
            // Save to API
            fetch("/api/metadata", {
              method: "POST",
              body: JSON.stringify(metadata),
            });
          }}
        >
          Save Changes
        </Button>
      </div>

      <Tabs value={activeTab} onValueChange={setActiveTab}>
        <TabsList className="grid w-full grid-cols-5">
          <TabsTrigger value="fields">Fields</TabsTrigger>
          <TabsTrigger value="pipelines">Pipelines</TabsTrigger>
          <TabsTrigger value="types">Types & Tags</TabsTrigger>
          <TabsTrigger value="branding">Branding</TabsTrigger>
          <TabsTrigger value="integrations">Integrations</TabsTrigger>
        </TabsList>

        <TabsContent value="fields" className="space-y-4">
          <FieldEditor
            title="Deal Fields"
            fields={metadata.dealFields}
            onChange={(fields) =>
              setMetadata({ ...metadata, dealFields: fields })
            }
          />
          <FieldEditor
            title="Contact Fields"
            fields={metadata.contactFields}
            onChange={(fields) =>
              setMetadata({ ...metadata, contactFields: fields })
            }
          />
          <FieldEditor
            title="Company Fields"
            fields={metadata.companyFields}
            onChange={(fields) =>
              setMetadata({ ...metadata, companyFields: fields })
            }
          />
        </TabsContent>

        <TabsContent value="pipelines">
          <PipelineEditor
            pipelines={metadata.pipelines}
            onChange={(pipelines) =>
              setMetadata({ ...metadata, pipelines })
            }
          />
        </TabsContent>

        <TabsContent value="types">
          <TypesEditor
            tags={metadata.tags}
            sources={metadata.sources}
            onTagsChange={(tags) => setMetadata({ ...metadata, tags })}
            onSourcesChange={(sources) =>
              setMetadata({ ...metadata, sources })
            }
          />
        </TabsContent>

        <TabsContent value="branding">
          <BrandingEditor
            branding={metadata.branding}
            onChange={(branding) => setMetadata({ ...metadata, branding })}
          />
        </TabsContent>

        <TabsContent value="integrations">
          <IntegrationsEditor
            integrations={metadata.integrations}
            onChange={(integrations) =>
              setMetadata({ ...metadata, integrations })
            }
          />
        </TabsContent>
      </Tabs>
    </div>
  );
}

// Field editor component
function FieldEditor({
  title,
  fields,
  onChange,
}: {
  title: string;
  fields: MetadataField[];
  onChange: (fields: MetadataField[]) => void;
}) {
  const [editingField, setEditingField] = useState<MetadataField | null>(null);

  const handleAdd = () => {
    const newField: MetadataField = {
      id: `field_${Date.now()}`,
      key: `custom_${Date.now()}`,
      label: "New Field",
      type: "text",
      order: fields.length + 1,
    };
    onChange([...fields, newField]);
    setEditingField(newField);
  };

  const handleSave = (field: MetadataField) => {
    onChange(fields.map((f) => (f.id === field.id ? field : f)));
    setEditingField(null);
  };

  const handleDelete = (fieldId: string) => {
    onChange(fields.filter((f) => f.id !== fieldId));
  };

  return (
    <Card>
      <CardHeader className="flex flex-row items-center justify-between">
        <CardTitle className="text-lg">{title}</CardTitle>
        <Button size="sm" onClick={handleAdd}>
          <Plus className="w-4 h-4 mr-1" />
          Add Field
        </Button>
      </CardHeader>
      <CardContent>
        <div className="space-y-2">
          {fields
            .sort((a, b) => a.order - b.order)
            .map((field) => (
              <div
                key={field.id}
                className="flex items-center justify-between p-3 border rounded-lg hover:bg-muted/50"
              >
                <div className="flex items-center gap-3">
                  <GripVertical className="w-4 h-4 text-muted-foreground cursor-move" />
                  <div>
                    <p className="font-medium">{field.label}</p>
                    <p className="text-xs text-muted-foreground">
                      {field.key} • {field.type}
                      {field.required && " • Required"}
                    </p>
                  </div>
                </div>
                <div className="flex gap-1">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setEditingField(field)}
                  >
                    Edit
                  </Button>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => handleDelete(field.id)}
                  >
                    <Trash className="w-4 h-4" />
                  </Button>
                </div>
              </div>
            ))}
        </div>
      </CardContent>

      {editingField && (
        <Dialog open onOpenChange={() => setEditingField(null)}>
          <DialogContent>
            <DialogHeader>
              <DialogTitle>Edit Field</DialogTitle>
            </DialogHeader>
            <FieldForm field={editingField} onSave={handleSave} />
          </DialogContent>
        </Dialog>
      )}
    </Card>
  );
}

// Field form
function FieldForm({
  field,
  onSave,
}: {
  field: MetadataField;
  onSave: (field: MetadataField) => void;
}) {
  const [formData, setFormData] = useState(field);

  return (
    <div className="space-y-4">
      <div className="grid grid-cols-2 gap-4">
        <div className="space-y-2">
          <Label>Label</Label>
          <Input
            value={formData.label}
            onChange={(e) =>
              setFormData({ ...formData, label: e.target.value })
            }
          />
        </div>
        <div className="space-y-2">
          <Label>Key</Label>
          <Input
            value={formData.key}
            onChange={(e) =>
              setFormData({ ...formData, key: e.target.value })
            }
          />
        </div>
      </div>

      <div className="space-y-2">
        <Label>Type</Label>
        <Select
          value={formData.type}
          onValueChange={(type: any) =>
            setFormData({ ...formData, type })
          }
        >
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="text">Text</SelectItem>
            <SelectItem value="number">Number</SelectItem>
            <SelectItem value="date">Date</SelectItem>
            <SelectItem value="select">Select</SelectItem>
            <SelectItem value="multiselect">Multi-select</SelectItem>
            <SelectItem value="boolean">Boolean</SelectItem>
            <SelectItem value="url">URL</SelectItem>
            <SelectItem value="email">Email</SelectItem>
            <SelectItem value="phone">Phone</SelectItem>
          </SelectContent>
        </Select>
      </div>

      {(formData.type === "select" || formData.type === "multiselect") && (
        <div className="space-y-2">
          <Label>Options (comma-separated)</Label>
          <Input
            value={formData.options?.join(", ")}
            onChange={(e) =>
              setFormData({
                ...formData,
                options: e.target.value.split(", ").filter(Boolean),
              })
            }
            placeholder="Option 1, Option 2, Option 3"
          />
        </div>
      )}

      <div className="flex items-center gap-2">
        <Switch
          checked={formData.required}
          onCheckedChange={(checked) =>
            setFormData({ ...formData, required: checked })
          }
        />
        <Label>Required field</Label>
      </div>

      <Button onClick={() => onSave(formData)} className="w-full">
        Save Field
      </Button>
    </div>
  );
}

// Pipeline editor (simplified)
function PipelineEditor({
  pipelines,
  onChange,
}: {
  pipelines: Pipeline[];
  onChange: (pipelines: Pipeline[]) => void;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Pipelines</CardTitle>
      </CardHeader>
      <CardContent>
        {pipelines.map((pipeline) => (
          <div key={pipeline.id} className="mb-6">
            <h3 className="font-semibold mb-3">{pipeline.name}</h3>
            <div className="flex gap-2 overflow-x-auto pb-2">
              {pipeline.stages.map((stage) => (
                <div
                  key={stage.id}
                  className="min-w-[150px] p-3 border rounded-lg"
                  style={{ borderTop: `3px solid ${stage.color}` }}
                >
                  <p className="font-medium">{stage.name}</p>
                  <p className="text-xs text-muted-foreground">
                    {stage.probability}% probability
                  </p>
                </div>
              ))}
            </div>
          </div>
        ))}
      </CardContent>
    </Card>
  );
}

// Types & tags editor
function TypesEditor({
  tags,
  sources,
  onTagsChange,
  onSourcesChange,
}: {
  tags: string[];
  sources: string[];
  onTagsChange: (tags: string[]) => void;
  onSourcesChange: (sources: string[]) => void;
}) {
  return (
    <div className="grid grid-cols-2 gap-4">
      <Card>
        <CardHeader>
          <CardTitle>Tags</CardTitle>
        </CardHeader>
        <CardContent>
          <Textarea
            value={tags.join("\n")}
            onChange={(e) => onTagsChange(e.target.value.split("\n"))}
            rows={10}
            placeholder="One tag per line"
          />
        </CardContent>
      </Card>
      <Card>
        <CardHeader>
          <CardTitle>Sources</CardTitle>
        </CardHeader>
        <CardContent>
          <Textarea
            value={sources.join("\n")}
            onChange={(e) => onSourcesChange(e.target.value.split("\n"))}
            rows={10}
            placeholder="One source per line"
          />
        </CardContent>
      </Card>
    </div>
  );
}

// Branding editor
function BrandingEditor({
  branding,
  onChange,
}: {
  branding: CRMMetadata["branding"];
  onChange: (branding: CRMMetadata["branding"]) => void;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Palette className="w-5 h-5" />
          Branding
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <Label>App Name</Label>
          <Input
            value={branding.appName}
            onChange={(e) =>
              onChange({ ...branding, appName: e.target.value })
            }
          />
        </div>
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label>Primary Color</Label>
            <div className="flex gap-2">
              <Input
                type="color"
                value={branding.primaryColor}
                onChange={(e) =>
                  onChange({ ...branding, primaryColor: e.target.value })
                }
                className="w-16"
              />
              <Input value={branding.primaryColor} readOnly />
            </div>
          </div>
          <div className="space-y-2">
            <Label>Accent Color</Label>
            <div className="flex gap-2">
              <Input
                type="color"
                value={branding.accentColor}
                onChange={(e) =>
                  onChange({ ...branding, accentColor: e.target.value })
                }
                className="w-16"
              />
              <Input value={branding.accentColor} readOnly />
            </div>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}

// Integrations editor
function IntegrationsEditor({
  integrations,
  onChange,
}: {
  integrations: CRMMetadata["integrations"];
  onChange: (integrations: CRMMetadata["integrations"]) => void;
}) {
  return (
    <div className="space-y-4">
      {Object.entries(integrations).map(([key, config]) => (
        <Card key={key}>
          <CardHeader>
            <CardTitle className="flex items-center gap-2 capitalize">
              <Plug className="w-5 h-5" />
              {key}
            </CardTitle>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="flex items-center gap-2">
              <Switch
                checked={config.enabled}
                onCheckedChange={(checked) =>
                  onChange({
                    ...integrations,
                    [key]: { ...config, enabled: checked },
                  })
                }
              />
              <Label>Enabled</Label>
            </div>
            {config.enabled && (
              <div className="space-y-2">
                <Label>Webhook URL</Label>
                <Input
                  value={config.webhookUrl || ""}
                  onChange={(e) =>
                    onChange({
                      ...integrations,
                      [key]: { ...config, webhookUrl: e.target.value },
                    })
                  }
                  placeholder="https://..."
                />
              </div>
            )}
          </CardContent>
        </Card>
      ))}
    </div>
  );
}
