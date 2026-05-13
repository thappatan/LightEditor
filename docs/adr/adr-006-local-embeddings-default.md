---
adr: 006
title: AI — Local Embeddings Default
date: 2026-05-13
status: accepted
supersedes: null
---

# ADR-006: AI — Local Embeddings Default

## Context
Codebase RAG (section 5.E) ต้องการ embedding model เพื่อ index source code ทุกไฟล์ใน workspace ปริมาณข้อมูล:
- Codebase ขนาดกลาง = 10k-100k chunks
- Re-index บ่อยตอนแก้ไฟล์

มีทางเลือก embedding:
- Cloud (OpenAI text-embedding-3, Cohere, Voyage): quality สูง, รวดเร็ว, แต่ส่ง code ขึ้น cloud + cost ต่อ token
- Local (BGE, Nomic, MiniLM via fastembed/ort): privacy + free + offline, quality พอใช้

## Decision
**Local embeddings เป็น default** — cloud embeddings เป็น opt-in toggle

Default model candidates: BGE-small-en, Nomic-embed-text (via `fastembed` หรือ `ort`)
Vector store: `lancedb` (local file)

## Alternatives Considered
- **Cloud default, local optional**: quality + ease แต่ขัด vision privacy + cost
- **Hybrid hash routing** (sensitive files local, others cloud): complexity สูง, hard to communicate to user
- **No RAG, rely on @ mentions only**: ลด feature scope; competitors มี RAG ครบ

## Consequences
- ผลดี:
  - Privacy: code ไม่ออกเครื่อง user (default)
  - Cost: ไม่มี per-token billing
  - Offline capability
  - Indicator ชัดเจนเมื่อ user opt-in cloud (data leaves device)
- ผลเสีย:
  - Quality embedding ต่ำกว่า cloud frontier
  - Indexing ใช้ CPU/GPU เครื่อง user (ช้ากว่า cloud)
  - Model size ต้อง ship (50-500MB) หรือ first-run download
- Trade-offs: ยอม quality ลดเล็กน้อยแลก privacy + cost; cloud opt-in สำหรับ user ที่ต้องการ quality สูง

## References
- Spec doc section 5.E.3 (Privacy)
- fastembed — https://github.com/Anush008/fastembed-rs
- BGE — https://huggingface.co/BAAI/bge-small-en-v1.5
- LanceDB — https://lancedb.com/
