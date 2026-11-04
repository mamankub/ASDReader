# ลายเซ็น ASDReader (ใช้ภายในองค์กร)

ไฟล์ติดตั้งและโปรแกรมของ ASDReader เซ็นด้วย certificate ส่วนตัว ("ASDReader") ที่ออกโดย root CA ส่วนตัว ("ASDReader Root CA")
ไม่ได้ออกโดยหน่วยงานรับรองสาธารณะ เครื่องที่ไม่ได้ติดตั้ง root นี้จึงยังแสดง "Unknown publisher" / SmartScreen
เครื่องที่ติดตั้งแล้วจะเปิดไฟล์ติดตั้งได้โดยไม่มีคำเตือน และตรวจได้ว่าไฟล์ไม่ถูกแก้ไขหลังเซ็น

**ใช้เฉพาะกับเครื่องที่เชื่อถือได้ขององค์กร** การติดตั้ง root นี้หมายถึงเชื่อถือทุกอย่างที่เซ็นด้วยกุญแจของมัน

## ลายนิ้วมือ (SHA-256) ของ ASDReader Root CA

```
49375E2AF6FCAA95B25F219370A7D72C1E40D5AD08540AC0BD5A6F47C2A11CB7
```

เทียบค่านี้กับที่ผู้ดูแลแจ้งให้ก่อนติดตั้งทุกครั้ง (สคริปต์ตรวจให้อัตโนมัติและปฏิเสธไฟล์อื่น)

## ติดตั้งในแต่ละเครื่อง (5–7 เครื่อง)

1. คัดลอกสองไฟล์นี้ไปไว้โฟลเดอร์เดียวกันในเครื่อง: `ASDReader-Root-CA.cer` และ `Install-ASDReaderCertificate.ps1`
2. คลิกขวาที่ PowerShell เลือก **Run as administrator** แล้วรัน
   ```
   Set-ExecutionPolicy -Scope Process Bypass
   .\Install-ASDReaderCertificate.ps1
   ```
3. ผลที่ถูกต้อง: `Added to Root`, `Added to TrustedPublisher`

ถอนออก: `.\Install-ASDReaderCertificate.ps1 -Remove`

### ติดตั้งพร้อมกันทุกเครื่องด้วย Group Policy (ถ้ามี Active Directory)

Computer Configuration ▸ Policies ▸ Windows Settings ▸ Security Settings ▸ Public Key Policies
นำเข้า `ASDReader-Root-CA.cer` ที่ **Trusted Root Certification Authorities** และ **Trusted Publishers**

## ตรวจว่าไฟล์ถูกเซ็น

คลิกขวาที่ไฟล์ `.msi` หรือ `ASDReader.exe` ▸ Properties ▸ Digital Signatures ต้องเห็นผู้เซ็นชื่อ **ASDReader**
หรือใน PowerShell: `Get-AuthenticodeSignature .\ASDReader.exe | Format-List`
(ในเครื่องที่ติดตั้ง root แล้ว `Status` เป็น `Valid`)

## สำหรับผู้ดูแล

| รายการ | ที่เก็บ / อายุ |
|---|---|
| Root CA (กุญแจส่วนตัว) | อยู่นอก repo ในโฟลเดอร์ส่วนตัวของผู้ดูแล `~/asdreader-signing/root-ca.key` ห้ามอัปโหลด หมดอายุ 2036-11-01 |
| Certificate เซ็นโปรแกรม | GitHub secrets `WINDOWS_CERTIFICATE` (pfx แบบ base64) และ `WINDOWS_CERTIFICATE_PASSWORD` หมดอายุ 2029-11-03 |
| Thumbprint (SHA-1) ผู้เซ็น | `0015645D64A0B4295350C920BCC9CBF6A032DBCE` (workflow ตรวจค่านี้ทุก build) |

- ลายเซ็นมี timestamp จึงยังใช้ได้หลัง certificate เซ็นโปรแกรมหมดอายุ
- ต่ออายุ: ออก certificate เซ็นโปรแกรมใหม่จาก root เดิม (ตามขั้นตอน `openssl` เดิม) แล้วเปลี่ยน secret และ thumbprint ใน
  `.github/workflows/release.yml` เครื่องในองค์กร **ไม่ต้องทำอะไรเพิ่ม** เพราะเชื่อถือ root อยู่แล้ว
- ถ้ากุญแจ Root CA หาย: ต้องสร้าง root ใหม่และติดตั้งใหม่ทุกเครื่อง **สำรองโฟลเดอร์ `~/asdreader-signing/` ไว้อย่างน้อยหนึ่งที่ที่ปลอดภัย**
- ถ้ากุญแจรั่ว: ถอน root ออกจากทุกเครื่อง (`-Remove`) แล้วสร้างชุดใหม่
