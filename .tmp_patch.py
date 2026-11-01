p='crates/carter-app-core/src/session.rs'
s=open(p,encoding='utf-8').read()
n=s.count('"## 主对话\n\n（主对话已经关闭')
s=s.replace('"## 主对话\n\n（主对话已经关闭','"## 主对话\n\n（主对话已经关闭')
m=s.count('join("\n")')
s=s.replace('join("\n")','join("\n")')
open(p,'w',encoding='utf-8').write(s)
print(n,m)
